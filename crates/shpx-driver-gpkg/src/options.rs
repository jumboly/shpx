//! GeoPackage 固有オプション。
//!
//! `WriteOpts` / `ReadOpts` に driver-specific 拡張機構が無いため、
//! テーブル名指定は URI クエリ `?table=...` か環境変数で受け取る（README/docs 参照）。

use shpx_core::{ReadOpts, Result, Uri, WriteOpts};

use crate::util::{driver_msg, DRIVER_NAME};

/// 環境変数: 入出力対象のテーブル名（feature テーブルが複数ある場合の優先指定）。
pub const ENV_TABLE: &str = "SHPX_GPKG_TABLE";

/// 環境変数: 出力時の feature テーブル名（書き出し用、入力で使う `ENV_TABLE` と独立に上書きしたい場合）。
/// 未指定なら出力ファイル名の stem (拡張子除去) をテーブル名として使う。
pub const ENV_OUT_TABLE: &str = "SHPX_GPKG_OUT_TABLE";

/// 解決済みの読み出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
    /// 読み出しテーブル名の明示指定（URI クエリ or 環境変数）。
    /// `None` の場合は reader が gpkg_contents を見て自動選択する。
    pub table: Option<String>,
    pub src_crs: Option<shpx_core::Crs>,
}

impl ResolvedReadOpts {
    pub fn resolve(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let table = resolve_table(uri, ENV_TABLE)?;
        Ok(Self {
            table,
            src_crs: opts.src_crs.clone(),
        })
    }
}

/// 解決済みの書き出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub table: Option<String>,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
        // 書き出し側は ENV_OUT_TABLE → ENV_TABLE → URI クエリ の順に見る。
        // 読み込みと同じ環境変数で書き先まで決まると pipeline 中に取り違えやすいので、
        // OUT 用を先に探し、なければ共通の TABLE にフォールバックする。
        let table = match std::env::var(ENV_OUT_TABLE) {
            Ok(v) if !v.is_empty() => Some(v),
            _ => resolve_table(uri, ENV_TABLE)?,
        };
        Ok(Self {
            table,
            on_loss: opts.on_loss,
            overwrite: opts.overwrite,
        })
    }
}

/// `path/to/file.gpkg?table=foo` のクエリ部分を最小パースして `table` キーを取り出す。
/// 見つからなければ環境変数 `env_key` を見る。空文字列は「未指定」扱い。
fn resolve_table(uri: &Uri, env_key: &str) -> Result<Option<String>> {
    if let Some(t) = parse_query_table(&uri.raw)? {
        return Ok(Some(t));
    }
    match std::env::var(env_key) {
        Ok(v) if !v.is_empty() => Ok(Some(v)),
        _ => Ok(None),
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
            // 簡易デコード: `+` → space と `%XX`（GPKG は通常 ASCII テーブル名）。
            return Ok(Some(shpx_rdb_common::percent_decode(v)));
        }
    }
    Ok(None)
}

/// `Uri::path` は `?table=...` 部分を含む raw 文字列を返すので、
/// ファイルシステム側に渡すパスとしては query を切り落とす必要がある。
#[must_use]
pub fn strip_query(path: &str) -> &str {
    match path.split_once('?') {
        Some((p, _)) => p,
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_extracts_table() {
        let v = parse_query_table("/tmp/a.gpkg?table=cities").unwrap();
        assert_eq!(v.as_deref(), Some("cities"));
    }

    #[test]
    fn parse_query_handles_multiple_keys() {
        let v = parse_query_table("/tmp/a.gpkg?foo=1&table=places&bar=2").unwrap();
        assert_eq!(v.as_deref(), Some("places"));
    }

    #[test]
    fn parse_query_empty_value_errors() {
        let err = parse_query_table("/tmp/a.gpkg?table=").unwrap_err();
        assert!(matches!(err, shpx_core::Error::Driver { .. }));
    }

    #[test]
    fn parse_query_without_question_returns_none() {
        let v = parse_query_table("/tmp/a.gpkg").unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn strip_query_removes_query_suffix() {
        assert_eq!(strip_query("/tmp/a.gpkg?table=x"), "/tmp/a.gpkg");
        assert_eq!(strip_query("/tmp/a.gpkg"), "/tmp/a.gpkg");
    }
}
