# OnLoss (損失変換) 仕様

shpx の各 driver は、ソースから出力先への変換時に「型・精度・座標系などが完全に再現できない」場面に遭遇しうる。CLI ではこれを `--on-loss=error|warn|skip` フラグで一括制御し、各 driver は自身が検出した損失パターンに「kind 識別子」を付けて 1 箇所のヘルパに通知する。

本文書は、`--on-loss` の意味と、driver × loss kind の組み合わせを 1 箇所に集約した参照資料である。各 driver 個別の docs (`docs/{POSTGIS,SQLSERVER,SPATIALITE,GPKG,FGB,GEOJSON,CSV}.md` など) からはこのファイルへリンクする。

## `--on-loss=error|warn|skip` の意味

| 値 | 振る舞い |
|---|---|
| `error`（既定）| `Error::OnLoss { kind, field }` を返して停止する。データ正しさが必須なパイプラインの既定 |
| `warn` | `tracing::warn!` で警告を出し、driver 既定の縮退戦略 (clip / 丸め / fallback 値の埋め込みなど) で処理を続行する |
| `skip` | 警告を出さずに、その列を出力から除外するか、その値を NULL に置き換える |

CLI からは `shpx convert <src> <dst> --on-loss=warn` のように指定する。指定が無ければ `error` 扱い。

`Skip` 経路の挙動は loss kind ごとに「列ごと除外」「値を NULL」「driver 固定 fallback (例: SRID=0)」のいずれかに分かれる。下表の各 kind の説明を参照。

## 動作表 (driver × loss kind)

| driver | kind 識別子 | 発火条件 | warn の縮退 / skip の振る舞い |
|---|---|---|---|
| shp | `binary-on-shp` | Arrow `Binary` / `LargeBinary` / `FixedSizeBinary` 列 | DBF に Binary を載せる手段が無いため両者とも列ごと除外 |
| shp | `decimal-precision-on-dbf` | `Decimal128` の precision が DBF Numeric 上限 (18) を超える | DBF の精度上限まで切り詰めて格納 (warn) / 列を上限精度で記録 (skip) |
| shp | `dbf-name-truncation` | DBF 列名が UTF-8 で 10 byte を超える | 10 byte で切って記録 |
| shp | `utf8-cp-unmappable` | DBF cpg encoding (`cp932` 等) でマップできない文字を含む UTF-8 値 | encoder の代替文字 (通常 `?`) で書く |
| shp | `utf8-length-on-dbf` | DBF Character 列の最大バイト長を超える UTF-8 値 | UTF-8 byte 境界で truncate して書く |
| shp | `prj-write-unsupported-epsg` | EPSG コード不明な CRS を `.prj` に書こうとした | `.prj` ファイルを書かない |
| shp | `timestamp-truncate-on-dbf` | Arrow `Timestamp` 列 | 日付部分のみ DBF Date 列へ書く (時刻情報を捨てる) |
| shp | `timestamp-tz-on-dbf` | Arrow `Timestamp` で tz 付き | UTC 換算後に日付部分のみ書く |
| shp | `z-on-shp` | Z 座標を含むジオメトリ (将来用、現状の中間表現が XY のみのため未発火) | Z を捨てる |
| shp | `m-on-shp` | M 座標を含むジオメトリ (同上) | M を捨てる |
| csv | `binary-on-csv` | Arrow `Binary` / `LargeBinary` 列 | warn 経路は base64 等を実装していないため列ごと除外 |
| csv | `structured-on-csv` | Arrow `List` / `Struct` / `Map` 列 | JSON 文字列化が未実装のため warn でも書き出し時に拒否、skip のみ列ごと除外 |
| geojson | `binary-on-geojson` | Arrow `Binary` / `LargeBinary` 列 | warn 経路は base64 等を実装していないため列ごと除外 |
| geojson | `structured-on-geojson` | Arrow `List` / `Struct` / `Map` 列 | JSON 構造化が未実装のため warn でも書き出し時に拒否、skip のみ列ごと除外 |
| geojson | `decimal-on-geojson` | Arrow `Decimal128(p, s)` 列 | RFC 7946 で `number` への昇格に精度損失が起きうるため列ごと除外 (warn / skip 共通) |
| geojson | `timestamp-precision-on-geojson` | `Timestamp(Nanosecond \| Microsecond, _)` 列 | RFC 7946 表現は ms 精度までしか保証できないため列ごと除外 |
| geojson | `uint64-overflow-on-geojson` | `UInt64` 値が `i64::MAX` を超える | JSON Number で表せないため文字列に降格して書く |
| geojson | `nonfinite-float-on-geojson` | `Float32/64` の `NaN` / `Infinity` (RFC 8259 で禁止) | JSON `null` を書く |
| gpkg | `decimal-on-gpkg` | Arrow `Decimal128(p, s)` 列 | SQLite 宣言型に Decimal が無いため `TEXT` に文字列化して格納 |
| gpkg | `uint64-overflow-on-gpkg` | `UInt64` 値が `i64::MAX` を超える | warn は `i64::MAX` で飽和、skip は `NULL` |
| gpkg | `missing-crs-on-gpkg` | CRS 解決不能 (EPSG 化できない / `Crs` が無い) | `srs_id=0` (Undefined geographic) で書き出し |
| spatialite | `decimal-on-spatialite` | Arrow `Decimal128` / `Decimal256` 列 | `TEXT` に文字列化して格納 |
| spatialite | `uint64-overflow-on-spatialite` | `UInt64` 値が `i64::MAX` を超える | warn は `i64::MAX` で飽和、skip は `NULL` |
| spatialite | `missing-crs-on-spatialite` | CRS 解決不能 | `srid=0` (SpatiaLite 慣習で unknown) で書き出し |
| fgb | `decimal-on-fgb` | Arrow `Decimal128` / `Decimal256` 列 | FGB の ColumnType に Decimal が無いため文字列降格 |
| fgb | `uint64-overflow-on-fgb` | `UInt64` 値が `i64::MAX` を超える | warn でも roundtrip しないため値を記録した上で警告のみ |
| fgb | `missing-crs-on-fgb` | CRS 解決不能 | header の `crs` field を未設定で書き出し |
| postgis | `missing-crs-on-postgis` | CRS 解決不能 | `srid=0` で書き出し (PostGIS は SRID 0 を unknown として許容) |
| sqlserver | `missing-crs-on-sqlserver` | CRS 解決不能 | `geometry` 列は `srid=0`、`geography` 列は SRID 必須のため `4326` フォールバック |

**Parquet driver (`shpx-driver-parquet`)** は v0.7 時点で `loss_kind` module を空のまま整備している。現状の writer は `coerce_types=false` 固定で `Timestamp(Nanosecond)` / `Decimal128(<= 38)` を完全保持し、shpx 中間表現 (`shpx_geom::wkb::Geom`) も XY のみのため Z/M も到達しない。`--parquet-coerce-types` 等のフラグや Z/M 対応を入れた段階で `precision-on-parquet` / `nanosecond-truncation-on-parquet` / `z-on-parquet` / `m-on-parquet` を順次追加する想定 (`crates/shpx-driver-parquet/src/util.rs` のヘルパは既に整備済み)。

**`--on-loss` 制御外の固定 warn**: 一部 driver は `--on-loss` 経由ではなく無条件に `tracing::warn!` を出して列を skip する経路を持つ。代表例: `unsupported-type-on-dbf` (SHP driver、DBF にマップできない Arrow 型)。これらは「データ損失ではなく明確な未対応」のため `--on-loss=error` でも停止しない。

## driver 別の詳細

各 driver の loss kind は `crates/shpx-driver-<name>/src/util.rs` の `pub mod loss_kind` にまとめて宣言され、driver 内の writer / reader / schema 変換から `apply_on_loss(kind, field, on_loss)` を経由して 1 箇所で `--on-loss` に応じた分岐を行う。

新規 loss kind を追加する場合は:

1. driver の `util.rs::loss_kind` に `pub const NAME_ON_DRIVER: &str = "kebab-case-name";` を追加
2. 検出箇所から `apply_on_loss(loss_kind::NAME_ON_DRIVER, field_name, on_loss)?` を呼ぶ
3. 戻り値の `Result<bool>` を見て: `Err(_)` は `error` 経路 (既に `?` で伝播)、`true` は warn 経路 (継続)、`false` は skip 経路 (列ごと除外 / NULL 化など)
4. 本文書の動作表に行を追加する

## 発火経路の仕組み (内部実装メモ)

中核は `crates/shpx-rdb-common/src/on_loss.rs` の `apply_on_loss` ヘルパ:

```rust
pub fn apply_on_loss(
    kind: &'static str,
    field: &str,
    on_loss: OnLoss,
    warn_fn: impl FnOnce(),
) -> Result<bool> {
    match on_loss {
        OnLoss::Error => Err(Error::OnLoss { kind: kind.into(), field: field.into() }),
        OnLoss::Warn => { warn_fn(); Ok(true) }
        OnLoss::Skip => Ok(false),
    }
}
```

`tracing::warn!(target: ...)` の `target:` は const 要求のため、driver 側に薄いラッパを置いて driver ごとに固定する:

```rust
// 例: crates/shpx-driver-gpkg/src/util.rs
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
        tracing::warn!(target: "shpx::gpkg", kind, field, "lossy conversion");
    })
}
```

この設計のため:

- `tracing` の filter 設定は `RUST_LOG=shpx::gpkg=warn` のように driver 単位で絞り込める
- driver crate を追加した際に `shpx_rdb_common::apply_on_loss` を呼ぶラッパを 1 つ書けば boilerplate は完結する
- `Skip` 経路の意味付け (列ごと除外 / 値 NULL / fallback 埋め込み) は driver 側の呼び出し元の責任

`shpx-rdb-common` という crate 名は RDB driver 共通ヘルパとして発足したが、実体はファイル driver から呼んでも問題ない純粋ユーティリティ集である (Parquet / GeoJSON / FGB / GPKG / SpatiaLite が利用)。

## 関連ドキュメント

- `docs/CRS.md` — 入力 / 出力での CRS 解決優先順位
- `docs/DATA_TYPES.md` — driver × Arrow 型のマッピング表
- `crates/shpx-rdb-common/src/on_loss.rs` — `apply_on_loss` ヘルパの実装
- `crates/shpx-core/src/error.rs` — `Error::OnLoss` の定義
