//! URI / パス入力の正規化。
//!
//! 入力には 2 系統がある:
//! 1. ローカルファイルパス（例: `/tmp/foo.shp`）— 拡張子から `scheme` を推論する
//! 2. URL 形式（例: `pg://user:pass@host/db?table=x`）— 先頭の `<scheme>://` から
//!    `scheme` を取り、driver 側で URL 全体を再パースする
//!
//! `pg` / `postgres` / `postgresql` は libpq の慣習で同義なので、ここで `pg` に
//! 正規化して driver 側の `supported_schemes` を 1 個に保つ。

/// 入出力対象のパスを表す。`scheme` はローカルパスなら拡張子、URL なら scheme から取られる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uri {
    /// Driver 識別に使うスキーム文字列（`shp`, `parquet`, `pg` など、小文字）。
    pub scheme: String,
    /// ユーザー入力の原文字列。URL の場合は URL 全体、ファイルパスの場合はパスをそのまま保持する。
    pub raw: String,
}

impl Uri {
    /// 文字列入力から [`Uri`] を作る。先頭が `<scheme>://` 形式なら URL として、
    /// それ以外なら拡張子推論でローカルパスとして解釈する。
    ///
    /// 拡張子もスキームも無い場合 `scheme` は空文字列になり、Driver 解決時にエラーとなる。
    pub fn from_path(p: impl Into<String>) -> Self {
        let raw: String = p.into();
        if let Some(scheme) = detect_url_scheme(&raw) {
            return Self {
                scheme: normalize_scheme(&scheme),
                raw,
            };
        }
        let scheme = std::path::Path::new(&raw)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Self { scheme, raw }
    }

    /// ファイルパスとしてアクセスする際のショートカット。
    /// URL の場合は raw（URL 全体）を返すので、driver 側で再パースする責務がある。
    pub fn path(&self) -> &str {
        &self.raw
    }

    /// この URI が URL 形式（`<scheme>://...`）かどうか。driver 側の分岐で使う。
    #[must_use]
    pub fn is_url(&self) -> bool {
        detect_url_scheme(&self.raw).is_some()
    }
}

/// 入力先頭が `<scheme>://` 形式なら scheme 部分を返す。
///
/// RFC 3986 §3.1 の `scheme = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )` に従い、
/// `[a-z][a-z0-9+.\-]*` の後に `://` が続く形を識別する。識別後の scheme は
/// 大文字小文字を区別せず小文字に揃える。
fn detect_url_scheme(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let first = bytes.first()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    let mut i = 1;
    while i < bytes.len() {
        let b = bytes[i];
        let allowed = b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.');
        if !allowed {
            break;
        }
        i += 1;
    }
    // `<scheme>` の直後が `://` であることを確認する。
    if i + 3 > bytes.len() {
        return None;
    }
    if &bytes[i..i + 3] != b"://" {
        return None;
    }
    Some(s[..i].to_ascii_lowercase())
}

/// libpq の慣習で `pg` / `postgres` / `postgresql` は同義。driver 側を 1 scheme に
/// 集約するためここで `pg` に正規化する。それ以外の scheme は素通し。
fn normalize_scheme(scheme: &str) -> String {
    match scheme {
        "postgres" | "postgresql" => "pg".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_path_extracts_lowercase_extension() {
        let u = Uri::from_path("/tmp/Sample.SHP");
        assert_eq!(u.scheme, "shp");
        assert_eq!(u.path(), "/tmp/Sample.SHP");
        assert!(!u.is_url());
    }

    #[test]
    fn from_path_with_no_extension_yields_empty_scheme() {
        let u = Uri::from_path("/tmp/no_extension");
        assert_eq!(u.scheme, "");
        assert!(!u.is_url());
    }

    #[test]
    fn from_path_recognizes_pg_url() {
        let u = Uri::from_path("pg://user:pass@host:5432/db?table=t");
        assert_eq!(u.scheme, "pg");
        assert!(u.is_url());
        assert_eq!(u.path(), "pg://user:pass@host:5432/db?table=t");
    }

    #[test]
    fn from_path_normalizes_postgres_aliases() {
        assert_eq!(Uri::from_path("postgres://h/db").scheme, "pg");
        assert_eq!(Uri::from_path("postgresql://h/db").scheme, "pg");
        assert_eq!(Uri::from_path("PostgreSQL://h/db").scheme, "pg");
    }

    #[test]
    fn from_path_keeps_unknown_url_scheme() {
        let u = Uri::from_path("mssql://host/db?table=t");
        assert_eq!(u.scheme, "mssql");
        assert!(u.is_url());
    }

    #[test]
    fn detect_url_scheme_rejects_relative_paths() {
        // `./foo.shp` や `../bar` は URL ではない（`scheme://` を持たない）。
        assert!(detect_url_scheme("./foo.shp").is_none());
        assert!(detect_url_scheme("../bar").is_none());
        assert!(detect_url_scheme("foo.shp").is_none());
    }

    #[test]
    fn detect_url_scheme_rejects_drive_letter_like_paths() {
        // Windows のドライブレター `C:\foo` は scheme://形式ではない。
        assert!(detect_url_scheme("C:foo").is_none());
        assert!(detect_url_scheme("C:\\foo\\bar").is_none());
    }

    #[test]
    fn detect_url_scheme_handles_compound_scheme() {
        // `git+ssh://` のような複合 scheme も RFC 3986 上は valid。
        assert_eq!(
            detect_url_scheme("git+ssh://host/repo"),
            Some("git+ssh".to_string())
        );
    }
}
