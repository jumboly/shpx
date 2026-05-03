# shpx 設計ドキュメント

本ドキュメントは shpx の中核設計をまとめたもの。マイルストーンは [ROADMAP.md](ROADMAP.md)、型マッピング詳細は [DATA_TYPES.md](DATA_TYPES.md)、CRS 詳細は [CRS.md](CRS.md) を参照。

## ゴール / 非ゴール

### ゴール

1. シェープファイル / GeoPackage / GeoParquet / PostGIS / SpatiaLite / SQL Server を無損失で相互変換
2. FlatGeobuf / GeoJSON / GeoJSONL (NDJSON) / CSV (WKT) も初期対応
3. 巨大データを Lazy（batch streaming）で扱う
4. 属性の順序・型（decimal, timestamptz, BLOB, 文字列エンコーディング）を保全
5. CRS 変換（PROJ 依存）対応
6. CLI 配布は `cargo install shpx` と GitHub Releases の OS 別バイナリ
7. ソース拡張プラグイン（`Driver` trait を実装して再ビルド）

### 非ゴール（v1 時点）

- ラスターデータ（GeoTIFF など）
- 実行時動的ロード（dylib/WASM）プラグイン
- GUI

## 実装言語と主要クレート

| 領域 | クレート |
|---|---|
| CLI | `clap` (derive) |
| Arrow/Parquet | `arrow`, `parquet`, `arrow-arith` |
| Shapefile | `shapefile` crate + 自前 DBF 拡張（cpg, decimal精度） |
| GPKG/SpatiaLite | `rusqlite` + 自前 GPKG binary/SpatiaLite blob コーデック |
| PostgreSQL/PostGIS | `tokio-postgres` + 自前 COPY BINARY エンコーダ |
| SQL Server | `tiberius` + 自前 staging 経由 bulk、v2 で MS-SSCLRT エンコーダ |
| PROJ | `proj` crate（libproj を static link 可） |
| ジオメトリ | `geo-types`, `geozero`, `wkb`, `wkt` |
| FlatGeobuf | `flatgeobuf` crate |
| 非同期ランタイム | `tokio` |
| ログ | `tracing` |
| 進捗 | `indicatif` |

## アーキテクチャ

```
┌────────────┐   ┌────────────────────┐   ┌──────┐   ┌──────────────┐
│Reader      │→ │Iterator<RecordBatch>│→ │CRS   │→ │Writer        │
│(Driver)    │   │  (Lazy, Schema付)  │   │trans │   │(Bulk/Row)    │
└────────────┘   └────────────────────┘   └──────┘   └──────────────┘
```

- **中間表現**: Arrow `Schema` + `RecordBatch` ストリーム
- **ジオメトリ**: `Binary` 列に WKB を入れ、カラムメタデータに `"shpx:geometry" = {"encoding":"WKB","crs":{...}}` を持たせる（GeoParquet の慣習に揃える）
- **型情報の流通**: Arrow スキーマの field metadata と Arrow の論理型で、属性順 / decimal(p,s) / timestamp unit/tz / utf8 を保全

## コアトレイト

```rust
pub trait Driver: Send + Sync {
    fn name(&self) -> &'static str;
    fn supported_schemes(&self) -> &[&'static str]; // "shp", "gpkg", "pg", "mssql", ...
    fn capabilities(&self) -> Capabilities;

    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>>;
    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>>;
}

pub trait LayerReader: Send {
    fn schema(&self) -> SchemaRef;
    fn crs(&self) -> Option<&Crs>;
    fn row_count_hint(&self) -> Option<usize>;
    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_>;
}

pub trait LayerWriter: Send {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

pub trait BulkLoadWriter: LayerWriter {
    fn bulk_write(
        &mut self,
        batches: &mut dyn Iterator<Item = Result<RecordBatch>>,
    ) -> Result<()>;
}

pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub random_access: bool,
    pub bulk_load: bool,
    pub supports_blob: bool,
    pub supports_decimal: bool,
    pub supports_timestamp_tz: bool,
    pub string_encoding: StringEncoding, // Fixed(Utf8) | Configurable(list)
    pub max_decimal_precision: Option<u8>,
}
```

`Capabilities` で各 Driver が何をサポートするかを宣言し、コアパイプラインが最適なパスを選択する（bulk load, encoding 変換, 損失警告など）。

## CLI 設計

```
shpx convert <src> <dst> [options]
shpx info <src>
shpx schema <src>           # Arrow schema を JSON 出力
shpx drivers                # 有効なドライバと capabilities
shpx repl                   # v2: 対話的モード
```

### URI スキームと拡張子推論

拡張子から推論可能なら URI 化不要。明示したい時は URI で。

```bash
# 拡張子推論
shpx convert parcels.shp parcels.gpkg
shpx convert parcels.gpkg parcels.parquet --reproject EPSG:4326

# URI 指定
shpx convert parcels.shp 'pg://user:pass@host:5432/db?table=public.parcels&create=if-not-exists'
shpx convert 'mssql://host/db?table=dbo.cities&trusted_connection=true' cities.parquet
shpx convert 'sqlite://./data.gpkg?table=roads' 'pg://host/db?table=roads'
shpx convert roads.fgb roads.geojsonl       # GeoJSON Lines (NDJSON)
shpx convert roads.shp roads.geojson        # 通常 GeoJSON FeatureCollection
```

### 共通オプション

- `--reproject <EPSG:xxxx | WKT2 | proj-string>`
- `--batch-size <N>` (既定 65536)
- `--encoding <codec>` (出力のみ)
- `--on-loss <error|warn|skip>` (既定 error)
- `--where '<SQL式>'` (RDB reader のみ)
- `--select col1,col2,...`
- `--insert-mode <bulk|batch>` (RDB writer)
- `--create-table <if-not-exists|always|never>` (RDB writer)
- `--progress`
- `-j <N>` (reproject/encoding 並列数, 既定 CPU数)
- `--overwrite`
- `-v` / `-vv`

### 拡張子 / スキーム → Driver

| 拡張子/スキーム | Driver | 備考 |
|---|---|---|
| `.shp` | shapefile | `.shx`/`.dbf`/`.prj`/`.cpg` セット |
| `.gpkg`, `gpkg://` | gpkg | GPKG / SpatiaLite は URI scheme で完全に分離（同一 SQLite ファイルでも内容で自動振り分けはしない） |
| `.parquet` | parquet | GeoParquet 準拠 |
| `.fgb` | flatgeobuf | |
| `.geojson` | geojson | FeatureCollection |
| `.geojsonl`, `.ndjson`, `.jsonl` | geojsonl | 1 feature/行 |
| `.csv`, `.tsv` | csv | WKT 列名は `--geom-col` で指定 |
| `pg://`, `postgres://`, `postgresql://` | postgis | |
| `mssql://`, `sqlserver://` | sqlserver | |
| `.sqlite`, `.db`, `.spatialite`, `sqlite://`, `db://`, `spatialite://` | spatialite | v0.5 で `sqlite` scheme を SpatiaLite が専有。`?mod_spatialite=true` フラグ運用は採用しない。content-sniffing による自動振り分けは v1.0 以降の検討事項 |

## プラグイン機構

v1 はソース拡張のみ:

```rust
// crates/shpx-driver-fgb/src/lib.rs
use shpx_core::{Driver, inventory};

pub struct FgbDriver;

impl Driver for FgbDriver {
    // ...
}

inventory::submit! {
    Box::new(FgbDriver) as Box<dyn Driver>
}
```

`inventory` クレートで起動時に Driver を登録。ユーザは workspace に自作クレートを追加し、`shpx-cli` で feature flag を有効化 → `cargo build` で拡張完了。詳細は [CONTRIBUTING.md](CONTRIBUTING.md)。

## リポジトリ構成（cargo workspace）

```
shpx/
├── Cargo.toml (workspace)
├── crates/
│   ├── shpx-core/            # Driver trait, 中間型, Arrow helpers
│   ├── shpx-geom/            # WKB/WKT, CRS, reprojection
│   ├── shpx-rdb-common/      # RDB driver 共通ヘルパー (URI, on_loss, CRS→SRID 等)
│   ├── shpx-driver-shp/
│   ├── shpx-driver-gpkg/
│   ├── shpx-driver-spatialite/
│   ├── shpx-driver-parquet/
│   ├── shpx-driver-postgis/
│   ├── shpx-driver-sqlserver/
│   ├── shpx-driver-fgb/
│   ├── shpx-driver-geojson/
│   ├── shpx-driver-csv/
│   └── shpx-cli/             # clap CLI, バイナリクレート
├── tests/
│   ├── fixtures/
│   └── integration/
├── docs/
└── .github/workflows/         # CI: test, lint, release (cargo-dist)
```

## RDB Bulk Load 戦略

### PostgreSQL/PostGIS

- **COPY BINARY を自前実装**
  - PostgreSQL binary COPY format ヘッダ/trailer
  - 各型を pg binary 表現へエンコード（int, bigint, float8, text, bytea, numeric, timestamptz, geometry は EWKB を `bytea` 経由で送る → サーバ側で暗黙キャスト or `ST_GeomFromEWKB` 実行）
- `tokio-postgres::CopyInSink` を使う
- テーブルが無ければ `CREATE TABLE` を自動発行（`--create-table=if-not-exists|always|never`）
- 空間インデックスは後処理で `CREATE INDEX ... USING GIST` をオプション生成

### SQL Server

- **v1: Staging テーブル方式**
  1. `CREATE TABLE #shpx_stage_<uuid> (attr..., geom_wkb varbinary(max), geom_srid int)`
  2. `tiberius::Client::bulk_insert("#shpx_stage_<uuid>")` で WKB を流す（UDT 不使用なので tiberius の現状制約を回避）
  3. `INSERT INTO <target> (..., geom) SELECT ..., geometry::STGeomFromWKB(geom_wkb, geom_srid) FROM #shpx_stage_<uuid>`
  4. `DROP TABLE #shpx_stage_<uuid>`
  - chunk ごと（既定 1M 行）にコミットして tempdb パンク防止

- **v2: MS-SSCLRT エンコーダ実装** — UDT native binary を直接生成して `BulkLoadRequest` に乗せる。tiberius へ upstream PR も検討
- **フォールバック: prepared INSERT バッチ**（`--insert-mode=batch`）

### SQLite (GPKG / SpatiaLite)

- 単一トランザクション + prepared INSERT バッチ（既定 10K 行/コミット）
- `PRAGMA journal_mode=WAL` + `PRAGMA synchronous=NORMAL`
- GPKG: 作成時に `gpkg_contents`, `gpkg_geometry_columns`, `gpkg_spatial_ref_sys` を初期化
- SpatiaLite: `SELECT InitSpatialMetadata()`、`mod_spatialite` 動的ロード

## 損失変換ポリシー

- 既定 `--on-loss=error`（安全側）
- `--on-loss=warn`: 警告ログ + 可能な降格（decimal→double, timestamp→date, BLOB→skip 等）
- `--on-loss=skip`: 該当フィールドをスキップ

詳細は [DATA_TYPES.md](DATA_TYPES.md)。

## 重要な設計判断サマリ

| 項目 | 決定 | 理由 |
|---|---|---|
| 言語 | Rust | SQL Server (tiberius) + PROJ + Arrow の pure 実装が可能、単一バイナリ配布 |
| GDAL | 非依存 | バイナリサイズ、ビルド複雑性を排除 |
| 中間表現 | Arrow RecordBatch | 型保全（decimal, timestamptz, binary）、GeoParquet ネイティブ、streaming |
| プラグイン | ソース拡張 (Driver trait) | MVP シンプル、動的ロードは v2 以降 |
| CRS | EPSG 優先 + WKT2/PROJJSON | 各フォーマットの標準に合わせて出力変換 |
| Reprojection | 対応（PROJ 依存） | 実用上必須 |
| SQL Server bulk | v1=staging、v2=MS-SSCLRT | tiberius の現制約を回避しつつ段階的に最適化 |
| PostgreSQL bulk | 自前 COPY BINARY | 型制御と性能 |
| 損失変換 | デフォルト error | 安全側、明示的に warn/skip を選ぶ |
| 文字列中間 | UTF-8 | 出力時のみエンコーディング変換 |
| CLI 名 | shpx | 確定 |
| 配布 | crates.io + GitHub Releases | cargo-dist で OS 別バイナリ |

## 主要な参照先

- GeoParquet 仕様: https://geoparquet.org/
- GPKG 仕様: https://www.geopackage.org/spec/
- MS-SSCLRT: https://learn.microsoft.com/en-us/openspecs/sql_server_protocols/ms-ssclrt/
- PostgreSQL COPY binary format: https://www.postgresql.org/docs/current/sql-copy.html
- PROJ: https://proj.org/
- FlatGeobuf: https://flatgeobuf.org/
