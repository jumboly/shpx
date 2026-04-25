//! CSV/TSV 固有オプションの解決。
//!
//! コアの `ReadOpts` / `WriteOpts` には driver-specific な拡張が無いため、
//! v0.2 サイクル 1 では暫定で **環境変数経由** で受け取る。次サイクル以降
//! `WriteOpts.driver_specific: BTreeMap<String, String>` を導入し、CLI に
//! `--driver-opt KEY=VALUE` を生やす計画（`docs/CSV.md` の Future work 参照）。

use encoding_rs::{Encoding, UTF_8};
use shpx_core::{ReadOpts, Result, Uri, WriteOpts};

use crate::util::driver_msg;

/// 環境変数: 区切り文字 1 文字を上書きする。
pub const ENV_DELIMITER: &str = "SHPX_CSV_DELIMITER";
/// 環境変数: ヘッダ有無 (`true` / `false`)。既定 `true`。
pub const ENV_HAS_HEADER: &str = "SHPX_CSV_HAS_HEADER";
/// 環境変数: geometry 列名を明示する。
pub const ENV_GEOMETRY_COLUMN: &str = "SHPX_CSV_GEOMETRY_COLUMN";
/// 環境変数: BOM 出力ポリシー (`auto` / `always` / `never`)。
pub const ENV_BOM: &str = "SHPX_CSV_BOM";

/// BOM 出力ポリシー。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BomPolicy {
    /// UTF-8 出力時のみ BOM を付与する。
    Auto,
    /// 常に BOM を付与する（出力エンコーディングが UTF-8 の場合のみ意味がある）。
    Always,
    /// 一切付与しない。
    Never,
}

impl BomPolicy {
    fn parse(raw: &str) -> Result<Self> {
        match raw.to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "always" => Ok(Self::Always),
            "never" => Ok(Self::Never),
            other => Err(driver_msg(format!(
                "{ENV_BOM}: invalid value `{other}` (expected auto/always/never)"
            ))),
        }
    }
}

/// 解決済みの読み出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
    pub delimiter: u8,
    pub has_header: bool,
    pub geometry_column: Option<String>,
    pub encoding: &'static Encoding,
    pub src_crs: Option<shpx_core::Crs>,
}

impl ResolvedReadOpts {
    pub fn resolve(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let delimiter = resolve_delimiter(uri)?;
        let has_header = resolve_has_header()?;
        let geometry_column = std::env::var(ENV_GEOMETRY_COLUMN)
            .ok()
            .filter(|s| !s.is_empty());
        let encoding = resolve_encoding(opts.encoding.as_deref())?;
        Ok(Self {
            delimiter,
            has_header,
            geometry_column,
            encoding,
            src_crs: opts.src_crs.clone(),
        })
    }
}

/// 解決済みの書き出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub delimiter: u8,
    pub bom: BomPolicy,
    pub encoding: &'static Encoding,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
        let delimiter = resolve_delimiter(uri)?;
        let bom = resolve_bom()?;
        let encoding = resolve_encoding(opts.encoding.as_deref())?;
        Ok(Self {
            delimiter,
            bom,
            encoding,
            on_loss: opts.on_loss,
            overwrite: opts.overwrite,
        })
    }
}

fn resolve_delimiter(uri: &Uri) -> Result<u8> {
    if let Ok(raw) = std::env::var(ENV_DELIMITER) {
        let bytes = raw.as_bytes();
        if bytes.len() != 1 {
            return Err(driver_msg(format!(
                "{ENV_DELIMITER}: must be exactly 1 byte, got {} bytes",
                bytes.len()
            )));
        }
        return Ok(bytes[0]);
    }
    Ok(match uri.scheme.as_str() {
        "tsv" => b'\t',
        // .csv も明示されない拡張子も既定で `,`。
        _ => b',',
    })
}

fn resolve_has_header() -> Result<bool> {
    match std::env::var(ENV_HAS_HEADER) {
        Ok(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" => Ok(false),
            other => Err(driver_msg(format!(
                "{ENV_HAS_HEADER}: invalid value `{other}` (expected true/false)"
            ))),
        },
        Err(_) => Ok(true),
    }
}

fn resolve_bom() -> Result<BomPolicy> {
    match std::env::var(ENV_BOM) {
        Ok(raw) => BomPolicy::parse(&raw),
        Err(_) => Ok(BomPolicy::Auto),
    }
}

fn resolve_encoding(label: Option<&str>) -> Result<&'static Encoding> {
    match label {
        Some(s) => {
            lookup_label(s).ok_or_else(|| driver_msg(format!("unknown encoding label: {s}")))
        }
        None => Ok(UTF_8),
    }
}

/// SHP ドライバの `cpg::lookup_label` と同じ正規化テーブル。共通化は次サイクル課題。
fn lookup_label(label: &str) -> Option<&'static Encoding> {
    let normalized = label.trim().to_ascii_uppercase();
    let canonical: &str = match normalized.as_str() {
        "LATIN1" | "ISO-LATIN-1" | "ANSI" => "ISO-8859-1",
        "CP1252" | "WINDOWS-1252" | "WIN1252" => "windows-1252",
        "CP932" | "SJIS" | "SHIFTJIS" | "SHIFT-JIS" | "SHIFT_JIS" => "Shift_JIS",
        "UTF8" | "UTF-8" => "UTF-8",
        _ => label.trim(),
    };
    Encoding::for_label(canonical.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delimiter_defaults_to_comma_for_csv() {
        // 環境変数の影響を排除するため値を保存して復元する。
        let saved = std::env::var(ENV_DELIMITER).ok();
        std::env::remove_var(ENV_DELIMITER);
        let uri = Uri::from_path("/tmp/x.csv");
        assert_eq!(resolve_delimiter(&uri).unwrap(), b',');
        if let Some(v) = saved {
            std::env::set_var(ENV_DELIMITER, v);
        }
    }

    #[test]
    fn delimiter_is_tab_for_tsv() {
        let saved = std::env::var(ENV_DELIMITER).ok();
        std::env::remove_var(ENV_DELIMITER);
        let uri = Uri::from_path("/tmp/x.tsv");
        assert_eq!(resolve_delimiter(&uri).unwrap(), b'\t');
        if let Some(v) = saved {
            std::env::set_var(ENV_DELIMITER, v);
        }
    }

    #[test]
    fn bom_policy_parses() {
        assert_eq!(BomPolicy::parse("auto").unwrap(), BomPolicy::Auto);
        assert_eq!(BomPolicy::parse("ALWAYS").unwrap(), BomPolicy::Always);
        assert_eq!(BomPolicy::parse("never").unwrap(), BomPolicy::Never);
        assert!(BomPolicy::parse("bogus").is_err());
    }

    #[test]
    fn encoding_label_falls_back_to_utf8() {
        assert_eq!(resolve_encoding(None).unwrap(), UTF_8);
    }

    #[test]
    fn encoding_label_handles_aliases() {
        let enc = resolve_encoding(Some("cp932")).unwrap();
        assert_eq!(enc, encoding_rs::SHIFT_JIS);
    }
}
