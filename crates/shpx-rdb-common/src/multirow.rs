//! multi-row `VALUES` INSERT で 1 RPC に詰める chunk 行数の算出ヘルパ。
//!
//! SQL Server (param 上限 2100) や PostgreSQL (param 上限 65535) では 1 RPC あたりに
//! 詰められる param 数が決まっており、`INSERT INTO t (...) VALUES (?,?...), (?,?...), ...`
//! 形式で複数行を 1 RPC にまとめると round-trip を桁違いに削減できる。
//!
//! 各 driver は schema 確定時に `params_per_row` を計算し、本ヘルパで chunk 行数を求める。

/// 1 RPC あたりの param 上限を超えないように multi-row VALUES の chunk 行数を算出する。
///
/// `params_per_row`: 1 行あたり bind する param 数 (例: SQL Server は属性数 + 2 (WKB + SRID)、
///                   PostGIS は属性数 + 1 (EWKB))
/// `max_params_per_rpc`: driver 固有の上限 (SQL Server `2100`、PostgreSQL `65535`)
/// `safety_margin`: 上限への余白。tiberius / tokio_postgres が将来内部で予約する param に
///                  備えた defensive な値 (現状 16 で十分)
///
/// 戻り値は **必ず 1 以上** を保証する。`params_per_row > max_params_per_rpc` のような
/// 超ワイド schema では 1 行 1 RPC にフォールバックする (1 行も詰められないので
/// chunk loop は素の per-row INSERT と同じ shape になる)。
pub fn multirow_chunk_rows(
    params_per_row: usize,
    max_params_per_rpc: usize,
    safety_margin: usize,
) -> usize {
    let usable = max_params_per_rpc.saturating_sub(safety_margin);
    (usable / params_per_row.max(1)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlserver_typical_schema_packs_about_260_rows() {
        // 6 attrs + WKB + SRID = 8 params/row, SQL Server 上限 2100, margin 16
        assert_eq!(multirow_chunk_rows(8, 2100, 16), 260);
    }

    #[test]
    fn postgres_typical_schema_packs_about_9k_rows() {
        // 6 attrs + EWKB = 7 params/row, PostgreSQL 上限 65535, margin 16
        assert_eq!(multirow_chunk_rows(7, 65535, 16), 9359);
    }

    #[test]
    fn near_limit_schema_packs_one_row() {
        // params_per_row が usable に近いと 1 行ずつしか入らない
        assert_eq!(multirow_chunk_rows(2000, 2100, 100), 1);
    }

    #[test]
    fn over_wide_schema_falls_back_to_one_row() {
        // params_per_row > max のときは saturating_sub で usable=0、min 1 が効いて 1 行
        assert_eq!(multirow_chunk_rows(3000, 2100, 100), 1);
    }

    #[test]
    fn single_param_schema_uses_full_capacity() {
        // 1 param/row なら ~max まで詰め込める
        assert_eq!(multirow_chunk_rows(1, 2100, 100), 2000);
    }

    #[test]
    fn zero_params_per_row_does_not_panic() {
        // params_per_row=0 は実運用上ありえないが、`.max(1)` で 0 除算を回避する保証を確認
        assert_eq!(multirow_chunk_rows(0, 2100, 16), 2084);
    }
}
