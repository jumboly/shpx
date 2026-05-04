# tiberius 0.12.3 `bulk_insert` の Date encoding bug — 最小再現と分析

> **Status (2026-05-04)**: 修復済み。shpx は `jumboly/tiberius` の `shpx-patches`
> branch (commit `338e83a`) を `Cargo.toml` で git dep として参照しており、
> `bulk_all_types_together` を含む全 35 case の `bulk_repro` suite が緑。
> 修正の本体は `src/tds/codec/type_info.rs::VarLenContext::Encode` で `Daten` 分岐の
> `dst.put_u8(self.len() as u8)` 発行を削除する 1 行 patch (upstream closed PR #346 の
> backport)。同 fork branch には packet_size 改善 (upstream PR #400) も並行で取り込み済
> み。upstream prisma/tiberius に PR が merge されたら `crates.io` 版に戻す。

## TL;DR

**trigger は `Date32` 列ただ 1 つ**。`tiberius 0.12.3` の `bulk_insert` 経路で
`ColumnData::Date(Some(...))` (Rust `NaiveDate` を `IntoSql` 経由で渡したもの) を送ると、TDS
フレームの後続列 metadata が破壊され、後続列が「`Invalid column type from bcp client for
colid N`」として弾かれる。

minimal repro は **3 列 / 1 行** で踏む:

```text
schema = [id (Int64 NOT NULL), created (Date32 nullable), geom (Binary+SRID)]
rows   = 1
```

ROADMAP v1.x SQL Server セクション (旧記述: 「多列 + decimal + 連続 varbinary(max)」) は誤り。
Decimal / Binary / Timestamp / 列順 / chunk size / nullable / 列数のいずれも trigger 軸ではなかった。
Date32 を schema から外せば 11 列 + 全型網羅でも staging bulk が緑になる。

## 再現手順

```bash
docker compose up -d mssql
docker exec shpx-mssql /opt/mssql-tools18/bin/sqlcmd -S localhost -U sa -P 'Shpx_test_pw1!' -C \
    -Q "IF DB_ID(N'shpx_test') IS NULL CREATE DATABASE shpx_test;"

SHPX_TEST_SQLSERVER_URL='mssql://sa:Shpx_test_pw1!@localhost:1433/shpx_test?encrypt=false' \
    cargo test -p shpx-driver-sqlserver --test bulk_repro \
    -- --ignored --test-threads=1 --nocapture
```

各テストは `eprintln!` で `[bulk_repro/<case>] <schema> → Ok | FailColid(N) | Err(...)` を 1 行 dump。

## 観察 — 全 35 case のマトリックス (2026-05-04 計測, mssql-server:2022-latest x86_64
Rosetta on Apple Silicon, tiberius 0.12.3)

凡例: ✓ = bulk_write + finish + read-back 全成功 / ✗(N) = `Invalid column type ...
colid N` で失敗 / **太字** が trigger 列 (Date32)。`bulk_repro/<test_name>` は
`crates/shpx-driver-sqlserver/tests/bulk_repro.rs` の test fn 名。

### Fase 1 — Pairwise (3 trigger 候補型のペアワイズ)

| test | schema (attr 列のみ) | 結果 |
|---|---|---|
| `pair_decimal_timestamp_naive` | id, amount, event_at(naive) | ✓ |
| `pair_decimal_timestamp_utc` | id, amount, event_at(UTC) | ✓ |
| `pair_decimal_binary` | id, amount, payload | ✓ |
| `pair_timestamp_naive_binary` | id, event_at(naive), payload | ✓ |
| `pair_timestamp_utc_binary` | id, event_at(UTC), payload | ✓ |

→ Decimal / Timestamp(naive/UTC) / Binary はペアでもトリプルでも通る。

### Fase 2 — Triple minimal (3 型同時)

| test | schema | 結果 |
|---|---|---|
| `triple_minimal_naive` | id, amount, event_at(naive), payload | ✓ |
| `triple_minimal_utc` | id, amount, event_at(UTC), payload | ✓ |

→ 3 型同時でも通る。trigger は別軸。

### Fase 3 — 列数二分探索

| test | schema | 結果 |
|---|---|---|
| `triple_plus_3noise` | id, flag, class, score, amount, event_at(naive), payload (8 列) | ✓ |
| `triple_plus_5noise` | id, flag, class, score, name, tag, amount, event_at(naive), payload (10 列) | ✓ |
| `full_all_types_repro` | id, flag, class, score, name, tag, amount, **created(Date32)**, event_at(naive), payload (11 列) | ✗(9) |

→ 10 列 → 11 列で発火。差分は `created (Date32)` の追加のみ。**Date32 が trigger 軸候補**。

### Fase 4 — Sensitivity (列順 / 連続同型 / nullable / chunk size / 単独型)

minimal repro が確定する前の予備調査。Date32 を含まないため全て ✓。

| test | 結果 |
|---|---|
| `order_swap_amount_payload`, `order_amount_first`, `order_geom_before_payload` | ✓ |
| `binary_separated_by_int`, `binary_double`, `no_binary_payload` | ✓ |
| `triple_not_null` | ✓ |
| `chunk_size_one`, `chunk_size_three` | ✓ |
| `solo_binary`, `solo_timestamp_naive` | ✓ |

### Fase 5 — Date32 follow-up (trigger 軸の確定)

| test | schema | 結果 | 備考 |
|---|---|---|---|
| `pair_date_timestamp_naive` | id, **created**, event_at(naive) | ✗(3) | colid 3 = event_at (Date32 直後) |
| `solo_date32` | id, **created** | ✗(3) | colid 3 = `shpx_geom_wkb` (Date32 直後) |
| `triple_minimal_with_date32` | id, amount, **created**, event_at, payload | ✗(4) | colid 4 = event_at (Date32 直後) |
| `date32_before_timestamp_naive_only` | id, amount, **created**, event_at(naive) | ✗(4) | 同上 |
| `date32_before_timestamp_utc_only` | id, amount, **created**, event_at(UTC) | ✗(4) | tz-aware にしても変わらない |
| `date32_separated_from_timestamp` | id, amount, **created**, sep, event_at(naive) | ✗(4) | sep(Int64) が Date32 直後でも踏む |
| `date32_after_timestamp` | id, amount, event_at(naive), **created** | ✗(5) | colid 5 = `shpx_geom_wkb` (Date32 直後) |
| `full_minus_payload` | id..tag, amount, **created**, event_at | ✗(9) | colid 9 = event_at (Date32 直後) |
| `full_minus_decimal` | id..tag, **created**, event_at, payload | ✗(8) | colid 8 = event_at (Date32 直後) |

→ **すべての failing case で「colid 落ち位置 = Date32 ORDINAL_POSITION + 1」が完全一致**。
Date32 単独で trigger、後続列の何が来ても同じ症状。

### Fase 6 — Date32 minimal の確定

| test | schema | 結果 | 備考 |
|---|---|---|---|
| `solo_date32_not_null` | id, **created**(NN) | ✗(3) | nullable=false でも踏む |
| `solo_date32_one_row` (1 行) | id, **created** | ✗(3) | 1 行でも踏む (累積状態 bug ではない) |
| `date32_first_col` | **created**, geom | ✗(2) | id 無くても踏む |
| `date32_last_attr` | id, **created**, geom | ✗(3) | solo_date32 と同等 (再確認) |
| `double_date32` | id, **d1**, d2, geom | ✗(3) | 連続 Date32 で colid 報告は最初の Date32 直後 |

→ id / nullable / row 数いずれも trigger ではない。**最小再現 = Date32 列 1 つ + 後続に列 1 つ以上**。

## トリガー軸の確定

| 軸 | trigger 必要? | 根拠 |
|---|---|---|
| **Date32 (`date` 型) の `bulk_insert` での送信** | **必要** | Date32 を schema から抜くと全て ✓ |
| Date32 の直後に列が存在 | 必要 | 後続列フレームの破壊が症状 |
| Decimal128 / Timestamp / Binary | 不要 | Date32 抜きで全 type 網羅 ✓、Date32 単独で踏む |
| 列数 (≥ 11) | 不要 | 2-3 列でも踏む |
| 列順 (column-order) | 不要 | Date32 がどの位置でも、その直後で必ず踏む |
| 連続同型 (Binary 並び) | 不要 | binary_double / no_binary_payload とも Date32 抜きで ✓ |
| nullable=true (NULL marker) | 不要 | NN でも踏む |
| chunk size | 不要 | 1 行 / chunk=1 でも踏む |
| Timestamp の tz-naive vs tz-aware | 不要 | どちらも Date32 直後で踏むしどちらも単独 ✓ |

## 仮説 — tiberius 側のどこに bug があるか

`crates/shpx-driver-sqlserver/src/bulk.rs:118-131` の Date32 経路:

```rust
DataType::Date32 => {
    if is_null {
        Option::<NaiveDate>::None.into_sql()
    } else {
        let days = primitive::<Date32Type>(array, row);
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
        let d = epoch.checked_add_signed(Duration::days(i64::from(days)))...
        Some(d).into_sql()
    }
}
```

`NaiveDate::into_sql()` (tiberius 0.12 `IntoSql for NaiveDate`) は `ColumnData::Date(Some(...))`
を返す。tiberius の TDS encoder の `Date` variant 書き出しが、3 バイトの `date` 表現を送る際に
**送信長さの自己宣言を 1 バイト分多めにしている** か、**type info ヘッダのバイト数を誤っている**
のいずれかの可能性が高い (= 後続列の `colmetadata` を 1 バイト分食ってしまう)。

詳細特定には tiberius `src/tds/codec/column_data/date_time.rs` の `encode` 経路を読む必要がある
(本タスクのスコープ外)。upstream tiberius の関連 issue で同種の症状を報告するときの参考データ
として本ファイルを使える。

## upstream issue との照合

upstream prisma/tiberius には同種報告が 2 件 OPEN、修正 PR が 1 件 closed-unmerged で残って
いた:

- [Issue #373](https://github.com/prisma/tiberius/issues/373) (2025-01, 5 comments,
  OPEN): 「Date 列を抜くと通る」と報告。我々の観察と完全一致。
- [Issue #410](https://github.com/prisma/tiberius/issues/410) (2026-03, 0 comments,
  OPEN): minimal repro `(BIGINT, DATE, TIME(7))` で colid 3 失敗。BCP COLMETADATA の
  問題と分析、staging table workaround を提示。「DATE 直前 / TIME 直後」が trigger と
  記述されているが、本レポートの観察ではより広く「Date32 直後の列なら何でも踏む」。
- [PR #346 — fix: bulk insert date token error](https://github.com/prisma/tiberius/pull/346)
  (2024-07 提出 / 2026-05-02 review なしで auto-close): `VarLenContext::Encode` の
  `Daten` 分岐で `dst.put_u8(self.len())` を発行しない、約 10 行の極小 diff。
  closed-unmerged だが diff 自体は本 bug を直接修正する正しい patch。`reviewDecision:
  REVIEW_REQUIRED` / `reviews: []` / `headRepository: null` (作者 fork 削除) から、
  upstream 側のメンテ手不足で staleness auto-close されたものと推測。

## 採用した修正

shpx は **PR #346 を `jumboly/tiberius` の `shpx-bulk-date-fix` branch に backport**
した。修正 commit は [`d34cd76`](https://github.com/jumboly/tiberius/commit/d34cd76):

```rust
// src/tds/codec/type_info.rs::VarLenContext::Encode
match self.r#type {
    #[cfg(feature = "tds73")]
    VarLenType::Daten => {}                  // ← length バイトを書かない (固定 3 バイト)
    #[cfg(feature = "tds73")]
    VarLenType::Timen | VarLenType::DatetimeOffsetn | VarLenType::Datetime2 => {
        dst.put_u8(self.len() as u8);        // ← scale 依存の length は据え置き
    }
    ...
}
```

shpx 側の dep 切り替え:

```toml
# shpx workspace Cargo.toml
tiberius = { git = "https://github.com/jumboly/tiberius",
             rev = "338e83ae178be1763992316fdf234dc816392925",
             default-features = false,
             features = ["tds73", "rustls", "chrono", "rust_decimal"] }
```

`branch` ではなく `rev` で固定するのは fork side の force-push でも CI が静かに別コードを
走らない reproducibility のため。fork branch 名は `shpx-patches`、現 HEAD は PR #346 の
backport + PR #400 (LOGIN7 packet_size) の 2 patch を含む。

これにより本ファイルのマトリックスで failing だった 14 case はすべて Ok に変わり、
`bulk_all_types_together` の `#[ignore]` も解除済み (`tests/bulk_roundtrip.rs`)。fork
維持コストは Apache-2.0 license につき 0、保守工数のみ。upstream に修正がマージされ
次第 `crates.io` 版に戻す予定。

## 検討した shpx 側の対応選択肢 (歴史的経緯)

採用前に検討した 4 案を記録のため残す。最終的に **3 (fork して修正)** を選択した。

1. **Date32 列を bulk path で batch fallback に降格** — `writer.rs` / `bulk.rs` で
   schema に Date32 を見たら `BulkLoadWriter::bulk_write` を `LayerWriter::write` (1 行
   ずつ prepared INSERT) に切り替え。修正規模小、ただし `bulk_all_types_together` の
   `#[ignore]` は解除できず、bulk 速度が出ない。
2. **Date32 を Int32 (epoch days) で送信し staging で `CAST(... AS date)`** — tiberius を
   修正しない workaround。staging table 宣言型が `int` になるので `bulk_all_types_together`
   の bit-identical 期待値の再確認が必要。
3. **tiberius を fork して Date encoding 修正 (採用)** — Apache 2.0 / 約 10 行 patch。
   `bulk_all_types_together` の `#[ignore]` 解除と bench-rss bulk 復帰の両方を達成。
4. **odbc-api 移行** — 配布哲学 (system dep ゼロ) を崩す重い変更。v2.0 era の選択肢。

採用案 (3) の優位点: 修正が 1 関数 / 約 10 行で、shpx 側 driver 本体に縮退コードを残さずに
済む。upstream PR が既に存在 (#346) するため diff の正当性 review は不要、cherry-pick で完了。
fork 維持コストは Apache-2.0 license につき 0、保守工数のみ (upstream sync は半年に 1 度
再 rebase で十分)。

## 関連ファイル

- 再現テスト suite: `crates/shpx-driver-sqlserver/tests/bulk_repro.rs`
- 既存 `#[ignore]` テスト: `crates/shpx-driver-sqlserver/tests/bulk_roundtrip.rs:343-486`
- bulk encoder (Date32 経路): `crates/shpx-driver-sqlserver/src/bulk.rs:118-131`
- staging table CREATE / INSERT…SELECT: `crates/shpx-driver-sqlserver/src/staging.rs:104-220`
- arrow → SQL Server 型マッピング: `crates/shpx-driver-sqlserver/src/type_map.rs:17-55`
- ROADMAP v1.x 戦略: `docs/ROADMAP.md` の「SQL Server クライアント戦略 (decision tree)」セクション
