# shpx

GDAL 非依存・Rust 製の空間データ変換 CLI。多様な空間データ形式を Arrow `RecordBatch` ストリームを中間表現として無損失で相互変換する。本ドキュメントはこの文脈で使う言葉の用語集であり、実装仕様ではない。

## Language

### Driver / Format / Scheme

**Driver**:
1 つの読み書き戦略を表す実装単位（`Driver` trait の実装）。`name()`（小文字・ハイフン無し、例 `shp`, `gpkg`, `spatialite`, `postgis`）で一意に識別される。1 つの Driver は 1 つ以上の Scheme で解決され、1 つの Format（またはその特定プロファイル）を扱う。同一 Format に複数の Driver が存在しうる（後述の SQLite）。
_Avoid_: プラグイン, バックエンド, アダプタ

**Format**:
オンディスク／ワイヤ上のデータ形式そのもの（Shapefile, GeoPackage, GeoParquet, SQLite, PostGIS のテーブル等）。Driver と 1:1 ではない — **SQLite という単一 Format に対し gpkg Driver と spatialite Driver の 2 つが存在する**。どちらで開くかは内容の自動判別ではなく Scheme で決まる。
_Avoid_: ファイルタイプ, データソース形式

**Scheme**:
ユーザー入力文字列から Driver を解決するための鍵。ローカルパスなら拡張子（`shp`, `parquet`）、URL 形式なら `<scheme>://` の scheme 部（`pg`, `mssql`, `sqlite`）から取られ、小文字に正規化される。`postgres`/`postgresql` は `pg` に正規化される（libpq 慣習）。`sqlite://` scheme は spatialite Driver が専有する。
_Avoid_: プロトコル, プレフィックス

### Layer / Source

**Layer**:
1 回の読み書きが対象とする単一のフィーチャ集合 — 1 つの属性スキーマ、1 つのジオメトリ列、1 つの CRS を持つ。shpx は 1 回の `convert` につき必ず 1 Layer を読み 1 Layer を書く（複数 Layer の同時変換はしない）。
_Avoid_: テーブル, データセット, フィーチャクラス（いずれも文脈次第で Layer を指すが用語集では Layer に統一する）

**Source**:
1 つ以上の Layer を含みうる物理的な格納先 — ファイル（`.shp`, `.gpkg` ファイル）または DB 接続（PostGIS データベース等）。Layer ⊆ Source。Source 内の特定 Layer は `?table=<name>` で選ぶ。
_Avoid_: 入力, ファイル, データベース（Source の一形態にすぎない）

### 中間表現（Intermediate Representation / IR）

**中間表現（IR）**:
全 Driver が読み書きの両端で必ず経由する、Arrow `Schema` + `RecordBatch` のストリーム。属性順・型（decimal(p,s), timestamp の unit/tz, utf8）・ジオメトリ・CRS をここに集約することで、N 個の Format 間を N×N の個別変換器なしに無損失で相互変換できる。無損失性の保証点。
_Avoid_: パイプライン（IR を流す経路のことであり IR そのものではない）, Arrow ストリーム（実装名。文章では IR を使う）

**Geometry column（ジオメトリ列）**:
IR の中でジオメトリを保持する `Binary` 型の列。列メタデータ `"shpx:geometry"`（`{"encoding":"WKB","crs":{...}}`、GeoParquet 慣習）を持つ。1 Layer に 1 つ。
_Avoid_: geom 列, 図形列

**WKB**:
IR 内でのジオメトリのバイト表現。**中間表現の中ではジオメトリは常に WKB** で流れる（Format 固有のジオメトリ表現は Driver の境界で WKB に正規化される）。

**EWKB**:
PostGIS とのワイヤ表現（WKB に SRID を埋め込んだ拡張）。**PostGIS Driver の境界でのみ**現れ、IR 内の WKB とは区別する。
_Avoid_: WKB（SRID 埋め込みの有無で別物）

### 無損失変換の語彙

**Lossless（無損失）**:
IR が保持する全情報（属性順・型・ジオメトリ・CRS）を出力 Format が完全に再現できる状態。shpx の既定の前提であり、再現できない場合は既定（`--on-loss=error`）で変換を中断する。
_Avoid_: ロスレス, 完全変換

**Loss（損失）**:
出力 Format が IR のある型・値を表現できないという事実の発生。`--on-loss`（`error`/`warn`/`skip`、既定 `error`）の判定対象。
_Avoid_: 欠損, データロス

**Degradation（降格）**:
Loss を許容（`--on-loss=warn` または `skip`）した結果として適用する具体的な型の格下げ・フィールド除去（例: decimal→double, timestamp→date, BLOB→skip）。Loss が「表現できない事実」なのに対し、Degradation は「許容したうえで実際に行う代替」。
_Avoid_: フォールバック, 変換ロス

### 座標参照系の語彙

**CRS（座標参照系）**:
ジオメトリ座標がどの参照系に属するかを表す抽象概念（`Crs` 型）。authority（`EPSG:4326`）・WKT・PROJJSON の最大 3 形式を併せ持ち、等価判定は authority を優先する。Layer に高々 1 つ紐づく。
_Avoid_: 投影法, 座標系（投影法は CRS の構成要素にすぎない）

**SRID**:
RDB / GPKG が CRS を指すために持つ整数ハンドル（PostGIS `srid`, GPKG `srs_id`）。**CRS そのものではなく Format 内の参照キー**であり、同じ CRS でも Format が違えば別の整数になりうる。
_Avoid_: CRS, EPSG コード（EPSG コードは authority の一形態で、SRID と一致するとは限らない）

**Reprojection（再投影）**:
ジオメトリの座標値を別 CRS へ実際に変換する演算（PROJ 依存、`--reproject`）。**CRS メタデータの付け替え（座標は変えずラベルだけ変更）とは別物**。
_Avoid_: 変換, 座標変換（属性型変換と紛れる）

**WKT flavor**:
同一 CRS の WKT 表現の版。WKT1（OGC 01-009、Shapefile `.prj` の伝統形式）と WKT2（ISO 19162:2019、GeoParquet・現代 GPKG が要求）を区別し、出力 Format の要求に合わせて選び分ける。

### パス選択の語彙

**Capabilities**:
各 Driver が「自分が何をできるか」を宣言する構造体（`read`/`write`/`bulk_load`/`random_access`/`supports_decimal`/`supports_blob`/`supports_timestamp_tz`/`string_encoding`/`max_decimal_precision` 等）。コアパイプラインはこれを読んで書き込みパスを選び、エンコーディング変換を適用し、Loss を判定する。
_Avoid_: 機能フラグ, feature

**Bulk path（バルクロード）**:
`BulkLoadWriter` を実装する Driver が選ぶ高速書き込み経路（PostGIS `COPY BINARY` / SQL Server staging + `bulk_insert` / SQLite 単一トランザクション）。`Capabilities.bulk_load` が真のとき既定で選ばれる。
_Avoid_: 一括挿入

**Row path（行パス）**:
`write_batch` を逐次呼ぶ通常の書き込み経路。RDB では `--insert-mode=batch` で prepared INSERT バッチに落ちる。Bulk path を持たない Driver や `--insert-mode=batch` 指定時に使われる。
_Avoid_: 逐次パス, batch path（"batch" は別概念、下記参照）

**RecordBatch**:
IR の最小流通単位（Arrow の行群 + スキーマ）。全 Driver・全パスがこれを yield / 消費する。**`--insert-mode=batch` の "batch"（RDB の書き込みモード名）とは無関係**。
_Avoid_: バッチ（書き込みモードの batch と混同しない）

## Flagged ambiguities

- **「フォーマット」と「ドライバ」を同義で使わない。** 過去 CLAUDE.md / コードコメントには Driver を「フォーマット 1 種を表す」と書いた箇所があったが、SQLite（gpkg / spatialite）の存在からこれは厳密には誤り。Format ≠ Driver。
- **Layer 未指定時の解決はドライバ間で非対称。** GPKG は単一 feature テーブルなら自動採用・複数なら一覧付きエラー。RDB（PostGIS / SQL Server）は `?table=`（または環境変数）が常に必須で、先頭テーブルへのフォールバックはしない。
- **"batch" が二重定義。** `RecordBatch`（IR の最小流通単位、全経路で使う）と `--insert-mode=batch`（RDB writer が Bulk path ではなく prepared INSERT を使うモード名）は別概念。文章で「batch」と書くときはどちらか明示する。

## Example dialogue

> **Dev:** GPKG を PostGIS に入れる変換、どの Driver が動くんですか？
>
> **Domain expert:** 入力の Scheme が `.gpkg` だから gpkg Driver が解決される。同じ SQLite ファイルでも `sqlite://` で渡せば spatialite Driver になる — Format は同じ SQLite だけど Driver は別、ここは内容では振り分けない。
>
> **Dev:** その GPKG に複数テーブルがあったら？
>
> **Domain expert:** GPKG は Source に複数 Layer を持てる。1 つだけなら自動で採るけど複数あれば `?table=` で Layer を 1 つ選ばないとエラー。shpx は 1 変換 1 Layer だからね。PostGIS 側は逆に `?table=` 必須。
>
> **Dev:** 座標はそのまま入りますか？
>
> **Domain expert:** Reprojection は指定してないから座標値は変えない。GPKG の `srs_id`（SRID）が指す CRS を読んで、PostGIS 側の SRID に対応づけるだけ。CRS は同じ、ハンドルの整数が変わるだけ。
>
> **Dev:** decimal 列があるんですが PostGIS は受けますか？
>
> **Domain expert:** PostGIS の Capabilities は `supports_decimal=true` だから無損失で入る。これが Shapefile 出力なら decimal を表現しきれず Loss になって、既定の `--on-loss=error` で止まる。`warn` にすれば double への Degradation を許して続行する。
>
> **Dev:** 速度は？
>
> **Domain expert:** PostGIS は `bulk_load` を宣言してるから Bulk path（COPY BINARY）で入る。途中の RecordBatch は全経路共通の流通単位で、Bulk か Row かはそれとは別の話。ジオメトリは IR の中では WKB、PostGIS の境界で EWKB に変わる。
