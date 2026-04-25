//! URI / パス入力の正規化。
//!
//! v0.1 ではローカルファイルパスのみを受け取る。`pg://` `mssql://` `sqlite://` 等の
//! ネットワーク URI 解析は v0.3 以降で拡張する。
//! `scheme` は拡張子から推論し、Driver 解決のキーに使う。

/// 入出力対象のパスを表す。`scheme` は拡張子から推論される。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uri {
    /// Driver 識別に使うスキーム文字列（`shp`, `parquet` など、小文字）。
    pub scheme: String,
    /// ユーザー入力の原文字列。v0.1 ではローカルファイルパスをそのまま保持する。
    pub raw: String,
}

impl Uri {
    /// パス文字列から拡張子を推論して [`Uri`] を作る。
    /// 拡張子が無い場合 `scheme` は空文字列になり、Driver 解決時にエラーとなる。
    pub fn from_path(p: impl Into<String>) -> Self {
        let raw: String = p.into();
        let scheme = std::path::Path::new(&raw)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        Self { scheme, raw }
    }

    /// ファイルパスとしてアクセスする際のショートカット。
    /// 将来 RDB スキーム（`pg://`）等で raw とパス部分が分かれた時の差し替え点。
    pub fn path(&self) -> &str {
        &self.raw
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
    }

    #[test]
    fn from_path_with_no_extension_yields_empty_scheme() {
        let u = Uri::from_path("/tmp/no_extension");
        assert_eq!(u.scheme, "");
    }
}
