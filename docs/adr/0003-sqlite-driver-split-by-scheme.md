# SQLite ベースの GPKG / SpatiaLite を Scheme で分離し、内容での自動判別をしない

GeoPackage と SpatiaLite はどちらも SQLite ファイルだが、ジオメトリの blob 表現もメタデータテーブルも異なる。shpx は両者を **gpkg Driver / spatialite Driver の 2 つに分離**し、どちらで開くかを **Scheme（拡張子・URL scheme）で決める** — 同一 SQLite ファイルに対する content-sniffing による自動振り分けはしない。`.gpkg` は gpkg、`sqlite://` / `db://` / `spatialite://` は spatialite が専有する（`?mod_spatialite=true` のようなフラグ運用も採らない）。

理由は明示性と予測可能性。同じ `.sqlite` でも中身は GPKG のことも SpatiaLite のこともあり、内容推測で振り分けると誤判定や驚きを生む。Scheme で明示させれば、入力文字列だけから挙動が決まる。代償として利便性は下がる（ユーザーが Scheme を意識する必要がある）。「同一 Format に 2 Driver があるのに、なぜ自動で振り分けないのか」への回答としてここに残す。content-sniffing は v1.0 以降の検討事項として open。

## Consequences

- これは「Format ≠ Driver」という設計の最も顕著な実例（[CONTEXT.md](../../CONTEXT.md) 参照）。
