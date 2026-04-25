//! Shapefile `.prj` ファイル (WKT1) から EPSG コードを推定するヒューリスティクス。
//!
//! v0.1 は PROJ 非依存のため、WKT1 ⇄ EPSG の厳密な変換は行わない。
//! 代わりに WKT1 末尾に出現する `AUTHORITY["EPSG", N]` を正規表現で抽出する。
//! 失敗した場合は `Crs.wkt = Some(WKT1 原文)` のまま流通させる方針（呼び出し側で判断）。

use std::sync::OnceLock;

use regex::Regex;

/// WKT1 文字列から最も外側（= 末尾に出現する）の `AUTHORITY["EPSG", N]` を抽出する。
///
/// WKT1 の構造上、最も外側の CRS の AUTHORITY が文字列末尾に近い位置に書かれる。
/// 例:
///
/// ```text
/// PROJCS["WGS 84 / Pseudo-Mercator",
///   GEOGCS["WGS 84", DATUM[..., AUTHORITY["EPSG","6326"]], ..., AUTHORITY["EPSG","4326"]],
///   PROJECTION["Mercator_1SP"],
///   ...
///   AUTHORITY["EPSG","3857"]]   <- これを取りたい
/// ```
pub fn extract_epsg(wkt1: &str) -> Option<u32> {
    let re = epsg_regex();
    re.captures_iter(wkt1)
        .last()
        .and_then(|cap| cap.get(1)?.as_str().parse::<u32>().ok())
}

fn epsg_regex() -> &'static Regex {
    // 起動時 1 度だけコンパイル。
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // EPSG コードは数字、ダブルクォートは省略可（WKT1 の方言が複数あるため）。
        Regex::new(r#"AUTHORITY\s*\[\s*"EPSG"\s*,\s*"?(\d+)"?\s*\]"#)
            .expect("static regex must compile")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_4326_from_geogcs() {
        let wkt = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]]"#;
        assert_eq!(extract_epsg(wkt), Some(4326));
    }

    #[test]
    fn extracts_3857_from_projcs() {
        let wkt = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]],PROJECTION["Mercator_1SP"],AUTHORITY["EPSG","3857"]]"#;
        assert_eq!(extract_epsg(wkt), Some(3857));
    }

    #[test]
    fn extracts_jgd2011_6668() {
        let wkt = r#"GEOGCS["JGD2011",DATUM["Japanese_Geodetic_Datum_2011",SPHEROID["GRS 1980",6378137,298.257222101,AUTHORITY["EPSG","7019"]],AUTHORITY["EPSG","1128"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","6668"]]"#;
        assert_eq!(extract_epsg(wkt), Some(6668));
    }

    #[test]
    fn returns_none_when_no_authority() {
        let wkt = r#"PROJCS["unnamed",GEOGCS["unnamed",DATUM["unknown",SPHEROID["unknown",6378137,298.257223563]],PRIMEM["Greenwich",0],UNIT["degree",0.0174532925199433]],PROJECTION["Mercator_1SP"]]"#;
        assert_eq!(extract_epsg(wkt), None);
    }

    #[test]
    fn handles_unquoted_code() {
        // 一部の WKT1 方言ではコードがダブルクォート無し。
        let wkt = r#"GEOGCS["WGS 84",AUTHORITY["EPSG",4326]]"#;
        assert_eq!(extract_epsg(wkt), Some(4326));
    }
}
