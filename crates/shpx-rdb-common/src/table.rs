//! `schema.name` 形式のテーブル名を扱うヘルパー。
//!
//! - PostGIS は省略時 `public` を補う
//! - SQL Server は省略時 `dbo` を補う
//!
//! どちらも shape は同じなので `default_schema` を引数化して共通化する。

use shpx_core::{Error, Result};

/// `schema.name` を `(schema, name)` に分割。`.` が無ければ `default_schema` を返す。
///
/// ネストドット（`a.b.c`）は最初の `.` で分割される（`("a", "b.c")`）。SQL Server の
/// `database.schema.name` 3 段表記は driver 側で別途扱う想定。
#[must_use]
pub fn split_qualified(s: &str, default_schema: &str) -> (String, String) {
    if let Some((schema, name)) = s.split_once('.') {
        (schema.to_string(), name.to_string())
    } else {
        (default_schema.to_string(), s.to_string())
    }
}

/// テーブル名を解決する。優先順位: `query_table` > 環境変数 `env_var` > エラー。
///
/// `query_table` は URI クエリから既に取り出した値（percent decode 済み）を渡す。
/// 空文字は invalid として扱い、`Some("")` の場合もエラーになる。
///
/// driver 名はエラーメッセージに `Error::Driver` の `name` として含める。
pub fn resolve_table_name(
    query_table: Option<&str>,
    env_var: &'static str,
    driver_name: &'static str,
) -> Result<String> {
    if let Some(t) = query_table {
        if t.is_empty() {
            return Err(Error::driver_msg(
                driver_name,
                "empty `?table=` in URI query".to_string(),
            ));
        }
        return Ok(t.to_string());
    }
    match std::env::var(env_var) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(Error::driver_msg(
            driver_name,
            format!("?table=... query parameter is required (or set {env_var})"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_qualified_uses_default_when_no_dot() {
        assert_eq!(
            split_qualified("cities", "public"),
            ("public".to_string(), "cities".to_string())
        );
        assert_eq!(
            split_qualified("cities", "dbo"),
            ("dbo".to_string(), "cities".to_string())
        );
    }

    #[test]
    fn split_qualified_splits_on_first_dot() {
        assert_eq!(
            split_qualified("foo.bar", "public"),
            ("foo".to_string(), "bar".to_string())
        );
        // 3 段表記は最初の `.` で分割。`.` を含む側 (b.c) はそのまま name に入る。
        assert_eq!(
            split_qualified("a.b.c", "public"),
            ("a".to_string(), "b.c".to_string())
        );
    }

    #[test]
    fn resolve_table_name_uses_query_when_present() {
        let r = resolve_table_name(Some("dbo.cities"), "SHPX_TEST_TABLE", "test").unwrap();
        assert_eq!(r, "dbo.cities");
    }

    #[test]
    fn resolve_table_name_rejects_empty_query() {
        let err = resolve_table_name(Some(""), "SHPX_TEST_TABLE", "test").unwrap_err();
        assert!(format!("{err}").contains("empty"));
    }

    #[test]
    fn resolve_table_name_errors_when_neither_set() {
        // ユニークな env var 名を使い、外部で設定される可能性を実用上ゼロにする
        // （remove_var で他テストと衝突するリスクを避ける）。
        let env = "SHPX_RDB_COMMON_RESOLVE_TBL_NEVER_SET_42";
        if std::env::var(env).is_ok() {
            // 万一プロセス外から設定されていたらこのテストは skip する。
            return;
        }
        let err = resolve_table_name(None, env, "test").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("?table="), "msg was: {msg}");
        assert!(msg.contains(env), "msg was: {msg}");
    }
}
