//! Shapefile `.prj` ファイル (WKT1) の読み書き。
//!
//! Reader は WKT1 全文を `Crs.wkt` に詰め、`extract_epsg` で取れた EPSG を `authority` に展開する。
//! Writer は `epsg_to_wkt1` の hardcode テーブルから引き、未収録は `crs.wkt` フォールバックを使い、
//! それも無ければ `OnLoss` 適用で省略する。

use std::fs;
use std::path::Path;

use shpx_core::{Crs, OnLoss, Result, WktFlavor};
use shpx_geom::{epsg_to_wkt1, extract_epsg};

use crate::util::{apply_on_loss, loss_kind};

/// `<stem>.prj` を読み込み、`Crs` として返す。ファイルが無ければ `Ok(None)`。
pub fn read_prj(prj_path: &Path) -> Result<Option<Crs>> {
    let wkt = match fs::read_to_string(prj_path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let trimmed = wkt.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let authority = extract_epsg(trimmed).map(|code| ("EPSG".to_string(), code));
    Ok(Some(Crs {
        authority,
        wkt: Some(trimmed.to_string()),
        wkt_flavor: WktFlavor::V1,
        projjson: None,
    }))
}

/// `<stem>.prj` を書き出す。
///
/// 優先順位:
/// 1. `crs.epsg_code()` が `epsg_to_wkt1` の同梱テーブルに存在 → そのテーブルの WKT1 を書き出す
/// 2. `crs.wkt` (`wkt_flavor == V1`) が存在 → その文字列を書き出す
/// 3. いずれもダメ → `OnLoss` 適用で `.prj` を省略 (Skip/Warn) または中断 (Error)
pub fn write_prj(prj_path: &Path, crs: &Crs, on_loss: OnLoss) -> Result<()> {
    if let Some(code) = crs.epsg_code() {
        if let Some(wkt) = epsg_to_wkt1(code) {
            fs::write(prj_path, wkt)?;
            return Ok(());
        }
    }
    if let (Some(text), WktFlavor::V1) = (crs.wkt.as_deref(), crs.wkt_flavor) {
        fs::write(prj_path, text)?;
        return Ok(());
    }
    let field_label = crs
        .epsg_code()
        .map_or_else(|| "<unknown>".to_string(), |c| format!("EPSG:{c}"));
    if apply_on_loss(loss_kind::PRJ_WRITE_UNSUPPORTED_EPSG, &field_label, on_loss)? {
        // Warn: 警告は出したが出力スキップは確定。
    }
    // Skip / Warn 共に .prj ファイルは書かない。
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shpx_core::Error;

    #[test]
    fn read_prj_extracts_epsg_4326() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.prj");
        fs::write(&p, r#"GEOGCS["WGS 84",AUTHORITY["EPSG","4326"]]"#).unwrap();
        let crs = read_prj(&p).unwrap().unwrap();
        assert_eq!(crs.epsg_code(), Some(4326));
        assert_eq!(crs.wkt_flavor, WktFlavor::V1);
        assert!(crs.wkt.is_some());
    }

    #[test]
    fn read_prj_returns_none_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("missing.prj");
        assert!(read_prj(&p).unwrap().is_none());
    }

    #[test]
    fn write_prj_uses_known_epsg() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.prj");
        write_prj(&p, &Crs::from_epsg(4326), OnLoss::Error).unwrap();
        let s = fs::read_to_string(&p).unwrap();
        assert!(s.contains("AUTHORITY[\"EPSG\",\"4326\"]"));
    }

    #[test]
    fn write_prj_unsupported_epsg_errors_under_strict_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.prj");
        let err = write_prj(&p, &Crs::from_epsg(99999), OnLoss::Error).unwrap_err();
        assert!(matches!(err, Error::OnLoss { .. }));
        assert!(!p.exists());
    }

    #[test]
    fn write_prj_unsupported_epsg_skipped_under_skip_mode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.prj");
        write_prj(&p, &Crs::from_epsg(99999), OnLoss::Skip).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn write_prj_falls_back_to_wkt_field() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.prj");
        let crs = Crs {
            authority: None,
            wkt: Some("CUSTOM_WKT".to_string()),
            wkt_flavor: WktFlavor::V1,
            projjson: None,
        };
        write_prj(&p, &crs, OnLoss::Error).unwrap();
        assert_eq!(fs::read_to_string(&p).unwrap(), "CUSTOM_WKT");
    }
}
