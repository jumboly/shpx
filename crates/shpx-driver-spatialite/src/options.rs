//! SpatiaLite 固有オプション。`?table=` クエリと環境変数によるテーブル名指定。

use shpx_core::{ReadOpts, Result, Uri, WriteOpts};

use crate::util::{driver_msg, DRIVER_NAME};

pub const ENV_TABLE: &str = "SHPX_SPATIALITE_TABLE";
pub const ENV_OUT_TABLE: &str = "SHPX_SPATIALITE_OUT_TABLE";

#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
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

#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub table: Option<String>,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
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

fn resolve_table(uri: &Uri, env_key: &str) -> Result<Option<String>> {
    if let Some(t) = parse_query_table(uri.path())? {
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

/// `Uri::path` が含む `?table=...` 部分と、`sqlite://` の URL prefix を切り落とし、
/// SQLite が開けるファイルパスに整形する。
#[must_use]
pub fn strip_to_filepath(path: &str) -> &str {
    let no_query = match path.split_once('?') {
        Some((p, _)) => p,
        None => path,
    };
    // `sqlite://`, `db://`, `spatialite://` の URL prefix を取り除く。
    // `sqlite:///abs/path` → `/abs/path`、`sqlite://./rel` → `./rel`。
    for scheme in ["sqlite://", "db://", "spatialite://"] {
        if let Some(rest) = no_query.strip_prefix(scheme) {
            return rest;
        }
    }
    no_query
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_extracts_table() {
        let v = parse_query_table("/tmp/a.sqlite?table=cities").unwrap();
        assert_eq!(v.as_deref(), Some("cities"));
    }

    #[test]
    fn parse_query_handles_multiple_keys() {
        let v = parse_query_table("/tmp/a.sqlite?foo=1&table=places&bar=2").unwrap();
        assert_eq!(v.as_deref(), Some("places"));
    }

    #[test]
    fn parse_query_empty_value_errors() {
        let err = parse_query_table("/tmp/a.sqlite?table=").unwrap_err();
        assert!(matches!(err, shpx_core::Error::Driver { .. }));
    }

    #[test]
    fn parse_query_without_question_returns_none() {
        let v = parse_query_table("/tmp/a.sqlite").unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn strip_to_filepath_removes_query_and_scheme() {
        assert_eq!(strip_to_filepath("/tmp/a.sqlite?table=x"), "/tmp/a.sqlite");
        assert_eq!(strip_to_filepath("/tmp/a.sqlite"), "/tmp/a.sqlite");
        assert_eq!(strip_to_filepath("sqlite:///tmp/a.sqlite"), "/tmp/a.sqlite");
        assert_eq!(
            strip_to_filepath("sqlite://./rel.sqlite?table=t"),
            "./rel.sqlite"
        );
        assert_eq!(
            strip_to_filepath("spatialite:///tmp/x.sqlite"),
            "/tmp/x.sqlite"
        );
    }
}
