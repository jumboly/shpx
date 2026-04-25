//! サブコマンドの実装。

pub mod convert;
pub mod info;

use shpx_core::{Crs, Error, Result};

/// `EPSG:xxxx` 文字列を `Crs` にパースする。`None` 入力はそのまま `Ok(None)`。
pub fn parse_src_crs(s: Option<&str>) -> Result<Option<Crs>> {
    let Some(raw) = s else { return Ok(None) };
    Crs::parse_epsg(raw)
        .map(Some)
        .ok_or_else(|| Error::Crs(format!("invalid --src-crs: `{raw}` (expected EPSG:xxxx)")))
}
