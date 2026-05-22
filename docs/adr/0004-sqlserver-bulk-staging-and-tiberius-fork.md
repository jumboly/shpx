# SQL Server bulk load は staging テーブル方式とし、patch 済み tiberius fork に固定する

SQL Server への bulk load で、shpx は `geometry` 列へ直接 bulk するのではなく **staging `#temp` テーブル方式**を採る: WKB を `varbinary(max)` 列として `tiberius` の `bulk_insert` で流し込み、`INSERT … SELECT geometry::STGeomFromWKB(...)` で本テーブルへ移送して staging を drop する。理由は tiberius が SQL Server の UDT（`geometry` は UDT）への bulk を直接サポートしないため。WKB を経由すれば現状の制約を回避できる。

加えて、`tiberius` は crates.io 版ではなく **fork（`jumboly/tiberius#shpx-patches`、`rev` 固定）** に依存する。0.12.3 の `bulk_insert` 経路に `Date32` 列が 1 つでもあると後続列の TDS metadata が破壊されるバグがあり、`type_info.rs` の 1 行 patch で修正している（upstream closed PR #346 の backport、詳細は [SQLSERVER_BULK_BUG_REPRO.md](../SQLSERVER_BULK_BUG_REPRO.md)）。「なぜ公式版でなく fork を rev 固定で使うのか」「なぜ geometry に直接 bulk せず staging を経由するのか」への回答としてここに残す。

## Consequences

- fork 依存は保守負担になる。upstream prisma/tiberius に PR が merge され次第 crates.io 版へ戻す。
- staging は chunk ごとにコミットして tempdb の溢れを防ぐ。
- 将来 v2 で MS-SSCLRT エンコーダ（UDT native binary を直接生成）に移行する余地を残す。
