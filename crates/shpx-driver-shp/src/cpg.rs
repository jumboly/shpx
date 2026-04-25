//! `.cpg` ファイルの読み書きと、Reader/Writer 用エンコーディング解決。
//!
//! `.cpg` は DBF の文字列をデコードする際の codec 名 (例: `UTF-8`, `CP932`, `LATIN1`) を
//! 1 行のテキストで保存する Shapefile 慣習。`encoding_rs` の `Encoding::for_label` で正規化する。

use std::fs;
use std::path::Path;

use encoding_rs::{Encoding, UTF_8};
use shpx_core::{ReadOpts, Result, WriteOpts};

use crate::util::driver_msg;

/// `<stem>.cpg` を読み、`encoding_rs::Encoding` として返す。
/// ファイルが無ければ `Ok(None)`、未知ラベルなら `Err(Error::Driver)`。
pub fn read_cpg(cpg_path: &Path) -> Result<Option<&'static Encoding>> {
    let raw = match fs::read_to_string(cpg_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let label = raw.trim();
    if label.is_empty() {
        return Ok(None);
    }
    let enc = resolve_label(label, "unknown .cpg encoding label")?;
    Ok(Some(enc))
}

/// `.cpg` ファイルを 1 行で書き出す。
pub fn write_cpg(cpg_path: &Path, label: &str) -> Result<()> {
    fs::write(cpg_path, label)?;
    Ok(())
}

/// Reader 用エンコーディング解決。優先順位:
/// 1. `.cpg` ファイル
/// 2. `ReadOpts.encoding` 明示指定
/// 3. UTF-8 フォールバック
pub fn resolve_read_encoding(cpg_path: &Path, opts: &ReadOpts) -> Result<&'static Encoding> {
    if let Some(enc) = read_cpg(cpg_path)? {
        return Ok(enc);
    }
    if let Some(label) = opts.encoding.as_deref() {
        return resolve_label(label, "unknown read encoding label");
    }
    Ok(UTF_8)
}

/// Writer 用エンコーディング解決。`WriteOpts.encoding` が無ければ UTF-8。
pub fn resolve_write_encoding(opts: &WriteOpts) -> Result<&'static Encoding> {
    match opts.encoding.as_deref() {
        Some(label) => resolve_label(label, "unknown write encoding label"),
        None => Ok(UTF_8),
    }
}

fn resolve_label(label: &str, ctx: &str) -> Result<&'static Encoding> {
    lookup_label(label).ok_or_else(|| driver_msg(format!("{ctx}: {label}")))
}

/// `.cpg` 出力に使う正規化ラベル。`encoding_rs` の internal `name()` は IANA 名 (例: `Shift_JIS`) を返す。
pub fn cpg_label_for(enc: &'static Encoding) -> &'static str {
    enc.name()
}

/// `.cpg` ラベル文字列を `encoding_rs::Encoding` に解決する。
///
/// `encoding_rs::Encoding::for_label` は ASCII case-insensitive で多くの別名 (CP932/SJIS/Shift_JIS 等) を吸収する。
/// それでも `LATIN1` のような略号は通らないので、よく使う Shapefile 慣習名のエイリアスを補助テーブルで上書きする。
fn lookup_label(label: &str) -> Option<&'static Encoding> {
    let normalized = label.trim().to_ascii_uppercase();
    // よくある Shapefile/.cpg 方言を for_label が認識するラベルに正規化する。
    let canonical: &str = match normalized.as_str() {
        "LATIN1" | "ISO-LATIN-1" | "ANSI" => "ISO-8859-1",
        "CP1252" | "WINDOWS-1252" | "WIN1252" => "windows-1252",
        "CP932" | "SJIS" | "SHIFTJIS" | "SHIFT-JIS" | "SHIFT_JIS" => "Shift_JIS",
        "UTF8" | "UTF-8" => "UTF-8",
        // それ以外は label 原文を for_label に渡す。
        _ => label.trim(),
    };
    Encoding::for_label(canonical.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use encoding_rs::SHIFT_JIS;

    #[test]
    fn read_cpg_recognizes_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.cpg");
        fs::write(&p, "UTF-8").unwrap();
        let enc = read_cpg(&p).unwrap().unwrap();
        assert_eq!(enc, UTF_8);
    }

    #[test]
    fn read_cpg_recognizes_cp932() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.cpg");
        fs::write(&p, "CP932").unwrap();
        let enc = read_cpg(&p).unwrap().unwrap();
        assert_eq!(enc, SHIFT_JIS);
    }

    #[test]
    fn read_cpg_unknown_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.cpg");
        fs::write(&p, "NOT_AN_ENCODING").unwrap();
        let r = read_cpg(&p);
        assert!(r.is_err());
    }

    #[test]
    fn resolve_read_encoding_falls_back_to_utf8() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.cpg");
        let opts = ReadOpts::default();
        let enc = resolve_read_encoding(&p, &opts).unwrap();
        assert_eq!(enc, UTF_8);
    }

    #[test]
    fn resolve_read_encoding_uses_opts_when_no_cpg() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.cpg");
        let opts = ReadOpts {
            encoding: Some("cp932".to_string()),
            ..Default::default()
        };
        let enc = resolve_read_encoding(&p, &opts).unwrap();
        assert_eq!(enc, SHIFT_JIS);
    }
}
