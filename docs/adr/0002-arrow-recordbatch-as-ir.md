# Arrow RecordBatch ストリームを無損失の中間表現（IR）に採用する

shpx は N 個の Format 間を変換するが、N×N の個別変換器や geo-types を中心に据える代わりに、**Arrow `Schema` + `RecordBatch` ストリームを唯一の中間表現（IR）**とした。全 Driver は読み書きの両端で必ずこの IR を経由する。理由は無損失性と性能の両立にある — Arrow の論理型は decimal(p,s)・timestamp の unit/tz・binary・utf8 を表現でき、field metadata に属性順や CRS を載せられるため、型情報を失わずに Format 間を渡せる。GeoParquet がネイティブに Arrow を使うこと、`RecordBatch` 単位で streaming/lazy に流せること（データ全体を materialize しない）も決め手になった。

代償として、Arrow に第一級のジオメトリ型が無いため、ジオメトリは `Binary` 列の WKB として流し、列メタデータ `"shpx:geometry"` に encoding と CRS を持たせる GeoParquet 慣習に従う（多少不格好だが標準に沿う）。「なぜジオメトリが素の Binary 列なのか」「なぜ全経路が Arrow を通るのか」への回答としてここに残す。

## Consequences

- Arrow/Parquet のバージョンは workspace レベルで pin（公開クレート同士で `arrow_array` の型を共有するため揃える必要がある）。
- Driver 追加時は Format 固有表現 ↔ IR（Arrow + WKB）の双方向コーデックを実装すれば足り、他 Format との個別対応は不要。
