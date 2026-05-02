//! ドライバ共通ユーティリティ。PostGIS driver の `util.rs` と同形パターン。

use arrow_array::{Array, ArrowPrimitiveType, PrimitiveArray};
use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名（`Driver::name` 戻り値、`Error::Driver.name`、tracing target に使う）。
pub const DRIVER_NAME: &str = "sqlserver";

/// 損失種別の識別子。
pub mod loss_kind {
    /// CRS が無いデータを SQL Server に書く際の警告。geometry は srid=0、geography は
    /// 既定 4326（geography は有効な地理 CRS が必須なため fallback を分ける）にフォールバック。
    pub const MISSING_CRS_ON_SQLSERVER: &str = "missing-crs-on-sqlserver";
}

/// 任意の `Display` を `Error::Driver` に詰める。
pub fn driver_err<E: std::fmt::Display>(e: &E) -> Error {
    Error::driver(DRIVER_NAME, e)
}

/// 文字列メッセージから `Error::Driver` を作る。
pub fn driver_msg(msg: impl Into<String>) -> Error {
    Error::driver_msg(DRIVER_NAME, msg)
}

/// 損失検出時の挙動を 1 箇所で適用する。
///
/// 戻り値:
/// - `Ok(true)`  — 続行（`Warn` 経路）
/// - `Ok(false)` — その要素 (列・値) をスキップ
/// - `Err(_)`    — `OnLoss::Error` での中断
pub fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    match on_loss {
        OnLoss::Error => Err(Error::OnLoss {
            kind: kind.to_string(),
            field: field.to_string(),
        }),
        OnLoss::Warn => {
            tracing::warn!(target: "shpx::sqlserver", kind, field, "lossy conversion");
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

/// T-SQL 識別子（テーブル名・列名）を `[...]` でクオートする。
///
/// 内部の `]` は `]]` で二重化する（T-SQL の標準）。動的 SQL を組み立てる際に
/// ユーザー由来の名前をエスケープなしに展開すると SQL injection の温床になるため、
/// 必ず本関数を経由する。PostGIS driver の `"name"` 形式とは違い、SQL Server は
/// `[name]` をネイティブ識別子クオートとして使う。
#[must_use]
pub fn quote_ident(name: &str) -> String {
    let escaped = name.replace(']', "]]");
    format!("[{escaped}]")
}

/// `schema.name` 形式（または `name`）を quote 済みの完全修飾名にする。
/// schema 省略時は `dbo` を補う（SQL Server の既定スキーマ）。
#[must_use]
pub fn quote_qualified(schema: &str, table: &str) -> String {
    format!("{}.{}", quote_ident(schema), quote_ident(table))
}

/// T-SQL の文字列リテラル `N'...'` 用に `'` を `''` でエスケープする。
/// `N` プレフィックスで Unicode (nvarchar) 文字列リテラルとして解釈させる。
#[must_use]
pub fn quote_literal(s: &str) -> String {
    let escaped = s.replace('\'', "''");
    format!("N'{escaped}'")
}

/// `CREATE SPATIAL INDEX` の `WITH (BOUNDING_BOX = ...)` に埋め込む既知 EPSG 用の bbox。
///
/// SQL Server SPATIAL INDEX は `geometry` 列に対して BOUNDING_BOX が必須で、座標系の
/// 妥当な範囲を要求する（範囲外の geometry は index に乗らない）。同梱マップは
/// 4326 (WGS84 経緯度) と 3857 (Web Mercator) の 2 つに絞り、それ以外の SRID で
/// `--create-index=always` を指定した場合は明示エラーで利用者に bbox 設計を促す。
///
/// 値:
/// - 4326: 全球 `(-180, -90, 180, 90)` 経緯度
/// - 3857: Web Mercator の有効範囲 `(±20037508.34, ±20048966.10)` 相当を切り上げ
#[must_use]
pub fn bbox_for_epsg(code: u32) -> Option<(f64, f64, f64, f64)> {
    match code {
        4326 => Some((-180.0, -90.0, 180.0, 90.0)),
        3857 => Some((-20_037_508.34, -20_048_966.10, 20_037_508.34, 20_048_966.10)),
        _ => None,
    }
}

/// Arrow primitive 配列の指定行から native 値を取り出す。downcast 失敗は schema mismatch で
/// プログラムバグなので panic（呼び出し側で `DataType` を確認済みである前提）。
pub fn primitive<T: ArrowPrimitiveType>(array: &dyn Array, row: usize) -> T::Native {
    array
        .as_any()
        .downcast_ref::<PrimitiveArray<T>>()
        .expect("primitive downcast")
        .value(row)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_ident_uses_brackets() {
        assert_eq!(quote_ident("col"), "[col]");
    }

    #[test]
    fn quote_ident_doubles_internal_close_bracket() {
        // `]` のみがエスケープ対象。`[` は SQL Server 公式仕様上エスケープ不要。
        assert_eq!(quote_ident("a]b"), "[a]]b]");
    }

    #[test]
    fn quote_qualified_includes_schema() {
        assert_eq!(quote_qualified("dbo", "x"), "[dbo].[x]");
        assert_eq!(quote_qualified("my schema", "t"), "[my schema].[t]");
    }

    #[test]
    fn quote_literal_prefixes_n_and_doubles_quote() {
        assert_eq!(quote_literal("hello"), "N'hello'");
        assert_eq!(quote_literal("it's"), "N'it''s'");
    }

    #[test]
    fn apply_on_loss_error_returns_err() {
        let r = apply_on_loss(loss_kind::MISSING_CRS_ON_SQLSERVER, "x", OnLoss::Error);
        assert!(matches!(r, Err(Error::OnLoss { .. })));
    }

    #[test]
    fn apply_on_loss_warn_continues() {
        assert!(apply_on_loss(loss_kind::MISSING_CRS_ON_SQLSERVER, "x", OnLoss::Warn).unwrap());
    }

    #[test]
    fn apply_on_loss_skip_skips() {
        assert!(!apply_on_loss(loss_kind::MISSING_CRS_ON_SQLSERVER, "x", OnLoss::Skip).unwrap());
    }

    #[test]
    fn bbox_for_epsg_known_codes() {
        assert_eq!(bbox_for_epsg(4326), Some((-180.0, -90.0, 180.0, 90.0)));
        assert!(bbox_for_epsg(3857).is_some());
    }

    #[test]
    fn bbox_for_epsg_unknown_returns_none() {
        assert!(bbox_for_epsg(2451).is_none());
        assert!(bbox_for_epsg(0).is_none());
    }
}
