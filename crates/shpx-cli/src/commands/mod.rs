//! サブコマンドの実装。

pub mod convert;
pub mod drivers;
pub mod info;
pub mod schema;

use std::path::Path;

use shpx_core::{Crs, Driver, Error, LayerReader, ReadOpts, Result, Uri};

use crate::registry;

/// `EPSG:xxxx` 文字列を `Crs` にパースする。`None` 入力はそのまま `Ok(None)`。
pub fn parse_src_crs(s: Option<&str>) -> Result<Option<Crs>> {
    let Some(raw) = s else { return Ok(None) };
    Crs::parse_epsg(raw)
        .map(Some)
        .ok_or_else(|| Error::Crs(format!("invalid --src-crs: `{raw}` (expected EPSG:xxxx)")))
}

/// 共通の「path から driver を選んで reader を開く」処理。
///
/// `info` / `schema` 等の read 側サブコマンドが先頭で同じ手順を踏むため抽出した。
pub fn open_reader_for(
    src: &Path,
    src_crs: Option<&str>,
    encoding: Option<String>,
) -> Result<(&'static dyn Driver, Box<dyn LayerReader>)> {
    let uri = Uri::from_path(src.to_string_lossy().to_string());
    let driver = registry::select_driver(&uri).ok_or_else(|| registry::driver_not_found(&uri))?;
    let opts = ReadOpts {
        src_crs: parse_src_crs(src_crs)?,
        encoding,
    };
    let reader = driver.open_read(&uri, &opts)?;
    Ok((driver, reader))
}
