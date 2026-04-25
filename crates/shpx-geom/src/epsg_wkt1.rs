//! EPSG コードから WKT1 文字列への同梱テーブル。
//!
//! v0.1 は PROJ 非依存のため、Shapefile `.prj` 出力で使える EPSG は
//! ここに hardcode したものに限る。テーブル拡張は v0.2 で PROJ 統合と同時に行う。
//!
//! 現状の収録範囲（実用上の最頻出のみ）:
//! - 4326: WGS 84 (経緯度)
//! - 3857: WGS 84 / Pseudo-Mercator (Web Mercator)
//! - 4269: NAD83 (経緯度)
//! - 6668: JGD2011 (経緯度)

/// EPSG コードから WKT1 文字列を引く。テーブルに無ければ `None`。
pub fn epsg_to_wkt1(code: u32) -> Option<&'static str> {
    match code {
        4326 => Some(WKT1_4326),
        3857 => Some(WKT1_3857),
        4269 => Some(WKT1_4269),
        6668 => Some(WKT1_6668),
        _ => None,
    }
}

const WKT1_4326: &str = r#"GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AXIS["Latitude",NORTH],AXIS["Longitude",EAST],AUTHORITY["EPSG","4326"]]"#;

const WKT1_3857: &str = r#"PROJCS["WGS 84 / Pseudo-Mercator",GEOGCS["WGS 84",DATUM["WGS_1984",SPHEROID["WGS 84",6378137,298.257223563,AUTHORITY["EPSG","7030"]],AUTHORITY["EPSG","6326"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4326"]],PROJECTION["Mercator_1SP"],PARAMETER["central_meridian",0],PARAMETER["scale_factor",1],PARAMETER["false_easting",0],PARAMETER["false_northing",0],UNIT["metre",1,AUTHORITY["EPSG","9001"]],AXIS["X",EAST],AXIS["Y",NORTH],AUTHORITY["EPSG","3857"]]"#;

const WKT1_4269: &str = r#"GEOGCS["NAD83",DATUM["North_American_Datum_1983",SPHEROID["GRS 1980",6378137,298.257222101,AUTHORITY["EPSG","7019"]],AUTHORITY["EPSG","6269"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","4269"]]"#;

const WKT1_6668: &str = r#"GEOGCS["JGD2011",DATUM["Japanese_Geodetic_Datum_2011",SPHEROID["GRS 1980",6378137,298.257222101,AUTHORITY["EPSG","7019"]],AUTHORITY["EPSG","1128"]],PRIMEM["Greenwich",0,AUTHORITY["EPSG","8901"]],UNIT["degree",0.0174532925199433,AUTHORITY["EPSG","9122"]],AUTHORITY["EPSG","6668"]]"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wkt1_prj::extract_epsg;

    #[test]
    fn each_entry_roundtrips_through_extract_epsg() {
        // 同梱した WKT1 自身を extract_epsg に通すと元の EPSG が拾えること。
        for &code in &[4326_u32, 3857, 4269, 6668] {
            let wkt = epsg_to_wkt1(code).expect("entry exists");
            assert_eq!(extract_epsg(wkt), Some(code), "EPSG:{code} roundtrip");
        }
    }

    #[test]
    fn unknown_code_returns_none() {
        assert!(epsg_to_wkt1(99999).is_none());
    }
}
