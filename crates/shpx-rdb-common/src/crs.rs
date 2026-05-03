//! CRS → SRID（PostGIS / SQL Server で共通の整数 SRID）解決。
//!
//! 当 helper は CRS から EPSG 整数を i32 に詰めるところまでを引き受け、
//! 「CRS が無いときの fallback」「`spatial_ref_sys` への登録」「`on_loss` 適用」は
//! driver 側に残す（driver ごとに振る舞いが違うため）。

use shpx_core::{Crs, Error, GeometryMeta, Result};

/// `--src-crs` (CLI) > schema field metadata の優先で CRS を 1 つ採用する。
///
/// どちらも無ければ `None`。callee はこれを `resolve_epsg_srid` などに渡し、
/// SRID 解決と必要に応じた `spatial_ref_sys` 登録に使う。
#[must_use]
pub fn merge_crs(crs_arg: Option<&Crs>, geom_meta: &GeometryMeta) -> Option<Crs> {
    crs_arg.cloned().or_else(|| geom_meta.crs.clone())
}

/// CRS から EPSG 整数 SRID を取り出す。
///
/// 戻り値:
/// - `Ok(Some(srid))` — CRS から EPSG コードが取れた（i32 範囲内）
/// - `Ok(None)`       — CRS が `None`、または authority が EPSG 以外
/// - `Err(_)`         — EPSG コードが i32 範囲外
///
/// CRS 不在時の `OnLoss` 適用や fallback SRID（PostGIS = 0、SQL Server geography = 4326 等）
/// は呼び出し側で行う。
pub fn resolve_epsg_srid(crs: Option<&Crs>) -> Result<Option<i32>> {
    let Some(code) = crs.and_then(Crs::epsg_code) else {
        return Ok(None);
    };
    let srid = i32::try_from(code)
        .map_err(|_| Error::Crs(format!("EPSG code {code} exceeds i32 range")))?;
    Ok(Some(srid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shpx_core::{GeometryMeta, GeometryType};

    #[test]
    fn merge_prefers_crs_arg() {
        let arg = Crs::from_epsg(3857);
        let meta = GeometryMeta::wkb(GeometryType::Polygon, Some(Crs::from_epsg(4326)));
        let merged = merge_crs(Some(&arg), &meta).unwrap();
        assert_eq!(merged.epsg_code(), Some(3857));
    }

    #[test]
    fn merge_falls_back_to_meta() {
        let meta = GeometryMeta::wkb(GeometryType::Polygon, Some(Crs::from_epsg(4326)));
        let merged = merge_crs(None, &meta).unwrap();
        assert_eq!(merged.epsg_code(), Some(4326));
    }

    #[test]
    fn merge_returns_none_when_neither() {
        let meta = GeometryMeta::wkb(GeometryType::Polygon, None);
        assert!(merge_crs(None, &meta).is_none());
    }

    #[test]
    fn resolve_returns_some_for_epsg_crs() {
        let crs = Crs::from_epsg(4326);
        assert_eq!(resolve_epsg_srid(Some(&crs)).unwrap(), Some(4326));
    }

    #[test]
    fn resolve_returns_none_when_crs_is_none() {
        assert_eq!(resolve_epsg_srid(None).unwrap(), None);
    }

    #[test]
    fn resolve_returns_none_for_non_epsg_authority() {
        // ESRI:102100 のような non-EPSG authority は SRID 解決対象外。
        let mut crs = Crs::from_epsg(4326);
        crs.authority = Some(("ESRI".to_string(), 102_100));
        assert_eq!(resolve_epsg_srid(Some(&crs)).unwrap(), None);
    }

    #[test]
    fn resolve_errs_on_i32_overflow() {
        let mut crs = Crs::from_epsg(4326);
        crs.authority = Some(("EPSG".to_string(), u32::MAX));
        let err = resolve_epsg_srid(Some(&crs)).unwrap_err();
        assert!(matches!(err, Error::Crs(_)));
    }
}
