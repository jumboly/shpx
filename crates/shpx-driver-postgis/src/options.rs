//! PostGIS 固有オプション。
//!
//! テーブル名は URI クエリ `?table=schema.name` または `?table=name`、
//! あるいは環境変数 `SHPX_PG_TABLE` で指定する。schema 省略時は `public`。
//!
//! 本モジュールには PostGIS driver 固有の関心事のみを置く。汎用 URI / opts 解決は
//! `shpx_rdb_common` を参照。

use shpx_core::{CreateIndex, CreateTable, ReadOpts, Result, Uri, WriteOpts};
use shpx_rdb_common::{query_get, resolve_table_name, split_qualified, validate_overwrite_compat};

use crate::util::{driver_msg, DRIVER_NAME};

/// 環境変数: 入出力対象のテーブル名（`<schema>.<name>` 可）。URI クエリより優先度低。
pub const ENV_TABLE: &str = "SHPX_PG_TABLE";

/// PostGIS の既定スキーマ。`?table=` で schema を省略した場合に補う。
const DEFAULT_SCHEMA: &str = "public";

/// 解決済みの読み出しオプション（table モード向け）。
///
/// `--query` 指定時は `?table=` を解決しないため、reader 側で `ReadOpts` から
/// 直接読む（このオプション構造体は table モード経路でのみ生成する）。
#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
    /// 接続文字列（`pg://...`）。driver はこの URL を `tokio_postgres::Config` に渡す。
    pub url: String,
    /// テーブルが属する schema 名（既定 `public`）。
    pub schema: String,
    /// テーブル名。
    pub table: String,
    /// CLI `--src-crs` 由来の CRS 補完。
    pub src_crs: Option<shpx_core::Crs>,
}

impl ResolvedReadOpts {
    pub fn resolve(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let qualified = resolve_table(uri)?;
        let (schema, table) = split_qualified(&qualified, DEFAULT_SCHEMA);
        Ok(Self {
            url: uri.path().to_string(),
            schema,
            table,
            src_crs: opts.src_crs.clone(),
        })
    }
}

/// `--query` の SQL を最低限バリデーションする。
///
/// サブクエリ化（`SELECT ... FROM (<query>) AS shpx_q`）するため、`;` を含むと
/// 構文エラーになる。コメントや文字列リテラル中の `;` を厳密に判別するのは過剰なので、
/// 単純に「`;` を含めば reject」という保守的な方針を取る（ユーザーが SQL クライアントから
/// 末尾セミコロン付きでコピペしたケースを早めに弾くのが主目的）。
pub(crate) fn validate_user_query(q: &str) -> Result<()> {
    let trimmed = q.trim();
    if trimmed.is_empty() {
        return Err(driver_msg(format!("{DRIVER_NAME}: --query is empty")));
    }
    if trimmed.contains(';') {
        return Err(driver_msg(format!(
            "{DRIVER_NAME}: --query must not contain `;` (semicolons cannot be wrapped in a subquery)"
        )));
    }
    Ok(())
}

/// 解決済みの書き出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub url: String,
    pub schema: String,
    pub table: String,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
    pub create_table: CreateTable,
    pub create_index: CreateIndex,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
        validate_overwrite_compat(opts, DRIVER_NAME)?;
        let qualified = resolve_table(uri)?;
        let (schema, table) = split_qualified(&qualified, DEFAULT_SCHEMA);
        Ok(Self {
            url: uri.path().to_string(),
            schema,
            table,
            on_loss: opts.on_loss,
            overwrite: opts.overwrite,
            create_table: opts.create_table,
            create_index: opts.create_index,
        })
    }
}

/// テーブル名（`schema.name` または `name`）を解決する。
///
/// 優先順位: URI クエリ `?table=...` > 環境変数 `SHPX_PG_TABLE`。
/// どちらも無い場合はエラー（PostGIS は `--query` (cycle 3) 以外でテーブル名必須）。
fn resolve_table(uri: &Uri) -> Result<String> {
    let q_table = query_get(&uri.raw, "table");
    resolve_table_name(q_table.as_deref(), ENV_TABLE, DRIVER_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_read_extracts_table_from_query() {
        let uri = Uri::from_path("pg://h/db?table=cities");
        let r = ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).unwrap();
        assert_eq!(r.schema, "public");
        assert_eq!(r.table, "cities");
    }

    #[test]
    fn resolve_read_extracts_qualified_table() {
        let uri = Uri::from_path("pg://h/db?table=foo.bar");
        let r = ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).unwrap();
        assert_eq!(r.schema, "foo");
        assert_eq!(r.table, "bar");
    }

    #[test]
    fn resolve_read_decodes_percent_encoded_schema() {
        let uri = Uri::from_path("pg://h/db?table=my%20schema.name");
        let r = ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).unwrap();
        assert_eq!(r.schema, "my schema");
        assert_eq!(r.table, "name");
    }

    #[test]
    fn resolve_read_rejects_empty_table() {
        let uri = Uri::from_path("pg://h/db?table=");
        assert!(ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).is_err());
    }

    #[test]
    fn resolve_write_rejects_overwrite_with_never() {
        let uri = Uri::from_path("pg://h/db?table=t");
        let opts = WriteOpts {
            overwrite: true,
            create_table: CreateTable::Never,
            ..Default::default()
        };
        let err = ResolvedWriteOpts::resolve(&uri, &opts).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--overwrite"), "msg was: {msg}");
        assert!(msg.contains("--create-table=never"), "msg was: {msg}");
    }

    #[test]
    fn resolve_write_default_keeps_if_not_exists() {
        let uri = Uri::from_path("pg://h/db?table=t");
        let r = ResolvedWriteOpts::resolve(&uri, &WriteOpts::default()).unwrap();
        assert_eq!(r.create_table, CreateTable::IfNotExists);
        assert_eq!(r.create_index, CreateIndex::Auto);
    }

    #[test]
    fn resolve_table_missing_errors() {
        // 環境変数が他テストや shell から設定済みの環境では skip する。
        if std::env::var(ENV_TABLE).is_ok() {
            return;
        }
        let uri = Uri::from_path("pg://h/db");
        assert!(ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).is_err());
    }
}
