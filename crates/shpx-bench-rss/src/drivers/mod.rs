//! Driver 別の prepare + open_read 経路。
//!
//! `BenchDriver` trait を介して driver ごとの「Parquet 入力から native format への
//! 変換 (prepare)」と「native input から `LayerReader` を開く (open_read)」を抽象
//! 化する。`prepare_via_writer` / `prepare_via_bulk` を共通 helper として再利用し、
//! driver 個別 module は cache key と URI 形式の差分だけを記述する。

use std::path::{Path, PathBuf};

use shpx_core::{Driver, Error, LayerReader, OnLoss, ReadOpts, Result, Uri, WriteOpts};
use shpx_driver_parquet::ParquetDriver;

mod csv;
mod fgb;
mod geojson;
mod gpkg;
mod parquet;
mod postgis;
mod shp;
mod spatialite;
mod sqlserver;

/// Driver 固有の native 入力指定。
#[derive(Debug, Clone)]
pub enum NativeInput {
    /// File path 入力 (Parquet / SHP / FGB / CSV / GeoJSON / GPKG / SpatiaLite-file)。
    File(PathBuf),
    /// DB table 入力 (PostGIS / SQL Server)。`url` は table query を含まない base URL、
    /// `table` は対象テーブル名。`open_read` 側で `?table=` を追加する。
    DbTable {
        /// 接続 URL の base (table query 抜き)。
        url: String,
        /// 対象テーブル名。
        table: String,
    },
}

/// Driver 別の bench 経路を抽象化する trait。
pub trait BenchDriver {
    /// CLI `--driver` 引数で指定する識別子 (および出力 JSON の `driver` 値)。
    fn name(&self) -> &'static str;

    /// Parquet 入力ファイルから native format を準備する。
    /// manifest cache が valid であれば skip して `NativeInput` を返すだけにする。
    fn prepare(&self, parquet_input: &Path, dir: &Path, rows: usize) -> Result<NativeInput>;

    /// native input から reader を開く。
    fn open_read(&self, native: &NativeInput) -> Result<Box<dyn LayerReader>>;
}

/// CLI `--driver=<name>` から BenchDriver を解決する。
///
/// PostGIS / SQL Server は `--postgis-url` / `--sqlserver-url` (env: `SHPX_TEST_PG_URL`
/// / `SHPX_TEST_SQLSERVER_URL`) が必須。未指定なら呼び出し側に明示エラーを返す。
/// CLI `--driver=<name>` で受理する driver 識別子。
///
/// `clap::ValueEnum` 経由で文字列解釈は clap が担当する (kebab-case 自動変換)。
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum DriverKind {
    Parquet,
    Shp,
    Fgb,
    Csv,
    GeojsonFc,
    GeojsonNdjson,
    Gpkg,
    Spatialite,
    Postgis,
    Sqlserver,
}

pub fn dispatch(
    driver: DriverKind,
    postgis_url: Option<&str>,
    sqlserver_url: Option<&str>,
) -> Result<Box<dyn BenchDriver>> {
    match driver {
        DriverKind::Parquet => Ok(Box::new(parquet::ParquetBench)),
        DriverKind::Shp => Ok(Box::new(shp::ShpBench)),
        DriverKind::Fgb => Ok(Box::new(fgb::FgbBench)),
        DriverKind::Csv => Ok(Box::new(csv::CsvBench)),
        DriverKind::GeojsonFc => Ok(Box::new(geojson::GeoJsonBench(
            geojson::Variant::FeatureCollection,
        ))),
        DriverKind::GeojsonNdjson => Ok(Box::new(geojson::GeoJsonBench(geojson::Variant::Ndjson))),
        DriverKind::Gpkg => Ok(Box::new(gpkg::GpkgBench)),
        DriverKind::Spatialite => Ok(Box::new(spatialite::SpatialiteBench)),
        DriverKind::Postgis => {
            let url = postgis_url
                .ok_or_else(|| {
                    Error::Format(
                        "postgis driver requires --postgis-url or SHPX_TEST_PG_URL".into(),
                    )
                })?
                .to_string();
            Ok(Box::new(postgis::PostgisBench { url }))
        }
        DriverKind::Sqlserver => {
            let url = sqlserver_url
                .ok_or_else(|| {
                    Error::Format(
                        "sqlserver driver requires --sqlserver-url or SHPX_TEST_SQLSERVER_URL"
                            .into(),
                    )
                })?
                .to_string();
            Ok(Box::new(sqlserver::SqlserverBench { url }))
        }
    }
}

/// `dir/<key>.MANIFEST` が `rows=<N>\n` を保持していれば true を返す。
pub(crate) fn cache_valid(manifest: &Path, rows: usize) -> bool {
    let want = format!("rows={rows}\n");
    std::fs::read_to_string(manifest).is_ok_and(|s| s == want)
}

/// `base` URL/path に `?table=<name>` または `&table=<name>` を付与する。既存 query
/// (`?` を含む) があれば `&` で連結、無ければ `?` で開始する。
pub(crate) fn append_table_query(base: &str, table: &str) -> String {
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}table={table}")
}

/// `manifest` ファイルに `rows=<N>\n` を書き込む (cache 完了印)。
pub(crate) fn write_manifest(manifest: &Path, rows: usize) -> Result<()> {
    std::fs::write(manifest, format!("rows={rows}\n")).map_err(Error::Io)
}

/// Parquet 入力を file driver で複製する共通 helper。
///
/// `on_loss` は driver ごとに渡す (SHP は `Skip`, FGB / GPKG / SpatiaLite は `Warn`、
/// CSV / GeoJSON は `Skip` で Binary を落とす等、ロス挙動を呼び出し側で切り替える)。
pub(crate) fn prepare_via_writer(
    driver: &dyn Driver,
    parquet_input: &Path,
    out_uri: &Uri,
    on_loss: OnLoss,
) -> Result<()> {
    let pq_uri = Uri::from_path(parquet_input.display().to_string());
    let mut pq = ParquetDriver.open_read(&pq_uri, &ReadOpts::default())?;
    let schema = pq.schema();
    let crs = pq.crs().cloned();
    let opts = WriteOpts {
        overwrite: true,
        on_loss,
        ..Default::default()
    };
    let mut writer = driver.open_write(out_uri, schema, crs, &opts)?;
    {
        let iter = pq.batches();
        for batch in iter {
            let batch = batch?;
            writer.write_batch(&batch)?;
        }
    }
    writer.finish()?;
    Ok(())
}

/// Parquet 入力を RDB driver の bulk 経路で投入する共通 helper。
///
/// `open_bulk_write` が `None` を返した場合は通常 writer にフォールバック。RDB は
/// 全列ロスレス想定なので `OnLoss::Error`、テーブルは毎回 DROP→CREATE するため
/// `CreateTable::Always` を強制する。
pub(crate) fn prepare_via_bulk(
    driver: &dyn Driver,
    parquet_input: &Path,
    out_uri: &Uri,
) -> Result<()> {
    let pq_uri = Uri::from_path(parquet_input.display().to_string());
    let mut pq = ParquetDriver.open_read(&pq_uri, &ReadOpts::default())?;
    let schema = pq.schema();
    let crs = pq.crs().cloned();
    let opts = WriteOpts {
        overwrite: true,
        on_loss: OnLoss::Error,
        create_table: shpx_core::CreateTable::Always,
        ..Default::default()
    };
    if let Some(mut bulk) = driver.open_bulk_write(out_uri, schema.clone(), crs.clone(), &opts)? {
        let mut iter = pq.batches();
        bulk.bulk_write(&mut iter)?;
        bulk.finish()?;
    } else {
        let mut writer = driver.open_write(out_uri, schema, crs, &opts)?;
        let iter = pq.batches();
        for batch in iter {
            let batch = batch?;
            writer.write_batch(&batch)?;
        }
        writer.finish()?;
    }
    Ok(())
}
