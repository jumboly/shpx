//! CRS（座標参照系）の中間表現。
//!
//! v0.1 は PROJ 非依存のため、authority（EPSG 等）と元データから取得した WKT 文字列を保持する。
//! `wkt_flavor` で WKT1 / WKT2 を区別し、出力時のフォーマット要求に合わせて選び分ける。
//! 詳細な背景は `docs/CRS.md` 参照。

use serde::{Deserialize, Serialize};

/// 内部 CRS 表現。3 つの保持形式を持ち、利用可能なものから優先的に使う。
///
/// JSON 形式での field metadata 埋め込みを想定し、空フィールドは出力時に省略される。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Crs {
    /// `(authority, code)` の組。最頻出は `("EPSG", 4326)` など。
    /// 等価判定はこれが最も軽量・高速。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub authority: Option<(String, u32)>,

    /// 元データから取得した WKT 文字列（バージョンは `wkt_flavor` で区別）。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub wkt: Option<String>,

    /// `wkt` フィールドが WKT1 か WKT2 かを示す。
    #[serde(default)]
    pub wkt_flavor: WktFlavor,

    /// PROJ で解決した PROJJSON。v0.1 では authority のみの最小 PROJJSON を組み立てて使う。
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub projjson: Option<String>,
}

/// WKT のバージョン。
///
/// - WKT1 (OGC 01-009): Shapefile `.prj` の伝統的フォーマット
/// - WKT2 (ISO 19162:2019): GeoParquet / 現代 GPKG が要求するフォーマット
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WktFlavor {
    /// WKT1 (OGC 01-009)。
    V1,
    /// WKT2 (ISO 19162:2019)。新規データの既定。
    #[default]
    V2,
}

impl Crs {
    /// EPSG コードのみから [`Crs`] を作成する。
    pub fn from_epsg(code: u32) -> Self {
        Self {
            authority: Some(("EPSG".to_string(), code)),
            wkt: None,
            wkt_flavor: WktFlavor::V2,
            projjson: None,
        }
    }

    /// `EPSG:xxxx` 形式の文字列をパースする。
    /// 大文字小文字を区別しない。
    pub fn parse_epsg(s: &str) -> Option<Self> {
        let s = s.trim();
        let rest = s
            .strip_prefix("EPSG:")
            .or_else(|| s.strip_prefix("epsg:"))?;
        rest.parse::<u32>().ok().map(Self::from_epsg)
    }

    /// authority がある場合に EPSG 整数コードを返す。
    pub fn epsg_code(&self) -> Option<u32> {
        let (auth, code) = self.authority.as_ref()?;
        if auth.eq_ignore_ascii_case("EPSG") {
            Some(*code)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_epsg_handles_case_and_whitespace() {
        assert_eq!(
            Crs::parse_epsg("EPSG:4326").and_then(|c| c.epsg_code()),
            Some(4326)
        );
        assert_eq!(
            Crs::parse_epsg("epsg:3857").and_then(|c| c.epsg_code()),
            Some(3857)
        );
        assert_eq!(
            Crs::parse_epsg("  EPSG:6668  ").and_then(|c| c.epsg_code()),
            Some(6668)
        );
        assert!(Crs::parse_epsg("4326").is_none());
        assert!(Crs::parse_epsg("EPSG:abc").is_none());
    }

    #[test]
    fn epsg_code_requires_epsg_authority() {
        let mut c = Crs::from_epsg(4326);
        assert_eq!(c.epsg_code(), Some(4326));
        c.authority = Some(("ESRI".to_string(), 102_100));
        assert_eq!(c.epsg_code(), None);
    }
}
