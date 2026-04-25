//! PostGIS 固有オプション。
//!
//! テーブル名は URI クエリ `?table=schema.name` または `?table=name`、
//! あるいは環境変数 `SHPX_PG_TABLE` で指定する。schema 省略時は `public`。

use shpx_core::{CreateIndex, CreateTable, ReadOpts, Result, Uri, WriteOpts};

use crate::util::{driver_msg, DRIVER_NAME};

/// 環境変数: 入出力対象のテーブル名（`<schema>.<name>` 可）。URI クエリより優先度低。
pub const ENV_TABLE: &str = "SHPX_PG_TABLE";

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
        let (schema, table) = split_qualified(&qualified);
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
        // `--overwrite` は DROP→CREATE を要求するため `Never` (CREATE 発行禁止) と矛盾する。
        // CLI 段階で気付かせるため早期に reject する。
        if opts.overwrite && matches!(opts.create_table, CreateTable::Never) {
            return Err(driver_msg(format!(
                "{DRIVER_NAME}: --overwrite と --create-table=never は同時に指定できない"
            )));
        }
        let qualified = resolve_table(uri)?;
        let (schema, table) = split_qualified(&qualified);
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
    if let Some(t) = parse_query_table(&uri.raw)? {
        return Ok(t);
    }
    match std::env::var(ENV_TABLE) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(driver_msg(format!(
            "{DRIVER_NAME}: ?table=... query parameter is required (or set {ENV_TABLE})"
        ))),
    }
}

/// `schema.name` を `(schema, name)` に分割。schema 省略時は `public`。
fn split_qualified(s: &str) -> (String, String) {
    if let Some((schema, name)) = s.split_once('.') {
        (schema.to_string(), name.to_string())
    } else {
        ("public".to_string(), s.to_string())
    }
}

fn parse_query_table(raw: &str) -> Result<Option<String>> {
    let Some((_, query)) = raw.split_once('?') else {
        return Ok(None);
    };
    for pair in query.split('&') {
        let Some((k, v)) = pair.split_once('=') else {
            continue;
        };
        if k.eq_ignore_ascii_case("table") {
            if v.is_empty() {
                return Err(driver_msg(format!(
                    "{DRIVER_NAME}: empty `?table=` in URI query"
                )));
            }
            return Ok(Some(percent_decode(v)));
        }
    }
    Ok(None)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_extracts_simple_table() {
        let v = parse_query_table("pg://h/db?table=cities").unwrap();
        assert_eq!(v.as_deref(), Some("cities"));
    }

    #[test]
    fn parse_query_extracts_qualified_table() {
        let v = parse_query_table("pg://h/db?table=public.cities").unwrap();
        assert_eq!(v.as_deref(), Some("public.cities"));
    }

    #[test]
    fn parse_query_decodes_percent_encoded() {
        let v = parse_query_table("pg://h/db?table=my%20schema.name").unwrap();
        assert_eq!(v.as_deref(), Some("my schema.name"));
    }

    #[test]
    fn parse_query_empty_value_errors() {
        assert!(parse_query_table("pg://h/db?table=").is_err());
    }

    #[test]
    fn split_qualified_defaults_to_public() {
        assert_eq!(
            split_qualified("cities"),
            ("public".to_string(), "cities".to_string())
        );
        assert_eq!(
            split_qualified("foo.bar"),
            ("foo".to_string(), "bar".to_string())
        );
    }

    #[test]
    fn resolve_table_falls_back_to_env() {
        // 環境変数のテストはプロセス共有ステートを汚染するため別途 integration test で検証する。
        // 単体では URI クエリだけを確認する。
        let uri = Uri::from_path("pg://h/db?table=t");
        let resolved = ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).unwrap();
        assert_eq!(resolved.schema, "public");
        assert_eq!(resolved.table, "t");
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
        let resolved = ResolvedWriteOpts::resolve(&uri, &WriteOpts::default()).unwrap();
        assert_eq!(resolved.create_table, CreateTable::IfNotExists);
        assert_eq!(resolved.create_index, CreateIndex::Auto);
    }

    #[test]
    fn resolve_table_missing_errors() {
        // 環境変数が未設定でクエリも無ければエラー。テスト内で env var を unset するのは
        // プロセス共有ステートの問題があるため、env が設定済みの環境では skip する。
        if std::env::var(ENV_TABLE).is_ok() {
            return;
        }
        let uri = Uri::from_path("pg://h/db");
        assert!(ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).is_err());
    }
}
