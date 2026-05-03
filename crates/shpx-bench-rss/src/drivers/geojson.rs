//! GeoJSON driver の bench 経路。FeatureCollection (`.geojson`) と NDJSON
//! (`.geojsonl`) を `Variant` 列挙で 1 struct に統合する (cycle 7 完了基準が file
//! driver 区分で 2 entry 計測を求めるため、`dispatch` 側で 2 entry を作る)。
//!
//! Binary は不可、Decimal は文字列降格、CRS は EPSG:4326 固定。
//! PostGIS bench schema は EPSG:4326 で生成されているため reproject 不要。

use std::path::Path;

use shpx_core::{Driver, LayerReader, OnLoss, ReadOpts, Result, Uri};
use shpx_driver_geojson::GeoJsonDriver;

use super::shp::file_path;
use super::{cache_valid, prepare_via_writer, write_manifest, BenchDriver, NativeInput};

#[derive(Clone, Copy)]
pub enum Variant {
    /// `.geojson` (RFC 7946 FeatureCollection)
    FeatureCollection,
    /// `.geojsonl` (newline-delimited JSON、1 行 1 Feature)
    Ndjson,
}

impl Variant {
    fn driver_name(self) -> &'static str {
        match self {
            Self::FeatureCollection => "geojson-fc",
            Self::Ndjson => "geojson-ndjson",
        }
    }

    fn cache_prefix(self) -> &'static str {
        match self {
            Self::FeatureCollection => "geojson_fc",
            Self::Ndjson => "geojson_ndjson",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::FeatureCollection => "geojson",
            Self::Ndjson => "geojsonl",
        }
    }
}

pub struct GeoJsonBench(pub Variant);

impl BenchDriver for GeoJsonBench {
    fn name(&self) -> &'static str {
        self.0.driver_name()
    }

    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput> {
        let prefix = self.0.cache_prefix();
        let out = dir.join(format!("{prefix}_{rows}.{}", self.0.extension()));
        let manifest = dir.join(format!("{prefix}_{rows}.MANIFEST"));
        if !cache_valid(&manifest, rows) {
            let _ = std::fs::remove_file(&out);
            let uri = Uri::from_path(out.display().to_string());
            prepare_via_writer(&GeoJsonDriver, parquet_input, &uri, OnLoss::Skip)?;
            write_manifest(&manifest, rows)?;
        }
        Ok(NativeInput::File(out))
    }

    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>> {
        let path = file_path(native, self.0.driver_name())?;
        GeoJsonDriver.open_read(&Uri::from_path(path.display().to_string()), &ReadOpts::default())
    }
}
