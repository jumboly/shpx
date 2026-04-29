//! CLI から見える Driver の動的レジストリ（`inventory` ベース）。
//!
//! 各 driver crate が `shpx_core::inventory::submit!` で送ったエントリを
//! 起動時に 1 度だけ集約してキャッシュする。リンカ順に依存しない決定的な
//! 並びを得るため、`name()` で sort する。

use std::sync::OnceLock;

use shpx_core::{inventory, Driver, DriverRegistration, Error, Uri};

// `inventory::submit!` の副作用（Driver 自動登録）のためだけに driver crate を
// リンクする。これらを `use _` しないとリンカが未参照と判断して
// crate ごと strip し、`inventory::iter` が空になる。
use shpx_driver_csv as _;
use shpx_driver_fgb as _;
use shpx_driver_geojson as _;
use shpx_driver_gpkg as _;
use shpx_driver_parquet as _;
use shpx_driver_postgis as _;
use shpx_driver_shp as _;
use shpx_driver_sqlserver as _;

/// 全 Driver のキャッシュ。`OnceLock` で初回アクセス時に 1 度だけ集約する。
static DRIVERS: OnceLock<Vec<&'static dyn Driver>> = OnceLock::new();

fn collect_drivers() -> Vec<&'static dyn Driver> {
    let mut v: Vec<&'static dyn Driver> = inventory::iter::<DriverRegistration>
        .into_iter()
        .map(|r| r.driver)
        .collect();
    // 同一 scheme を複数 driver が宣言した場合の解決順を決定的にするため、name で sort。
    v.sort_by_key(|d| d.name());
    v
}

/// ビルド時に組み込まれている全 Driver を返す。
#[must_use]
pub fn all_drivers() -> &'static [&'static dyn Driver] {
    DRIVERS.get_or_init(collect_drivers)
}

/// URI のスキーム (拡張子) から該当 Driver を解決する。
#[must_use]
pub fn select_driver(uri: &Uri) -> Option<&'static dyn Driver> {
    let scheme = uri.scheme.as_str();
    if scheme.is_empty() {
        return None;
    }
    all_drivers()
        .iter()
        .copied()
        .find(|d| d.supported_schemes().contains(&scheme))
}

/// URI から driver が解決できなかった際の整形済みエラー。
/// scheme が空（拡張子なし）かどうかでメッセージを切り替える。
pub fn driver_not_found(uri: &Uri) -> Error {
    if uri.scheme.is_empty() {
        Error::Format(format!("{}: no extension; cannot infer driver", uri.path()))
    } else {
        Error::Format(format!("no driver for scheme `{}`", uri.scheme))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_driver_for_shp() {
        let d = select_driver(&Uri::from_path("/tmp/sample.shp"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "shp");
    }

    #[test]
    fn select_driver_for_unknown_returns_none() {
        assert!(select_driver(&Uri::from_path("/tmp/sample.unknown")).is_none());
    }

    #[test]
    fn select_driver_for_pathless_returns_none() {
        assert!(select_driver(&Uri::from_path("/tmp/no_extension")).is_none());
    }

    #[test]
    fn all_drivers_includes_shp() {
        assert!(all_drivers().iter().any(|d| d.name() == "shp"));
    }

    #[test]
    fn select_driver_for_parquet() {
        let d = select_driver(&Uri::from_path("/tmp/sample.parquet"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "parquet");
    }

    #[test]
    fn select_driver_for_csv() {
        let d = select_driver(&Uri::from_path("/tmp/sample.csv"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "csv");
    }

    #[test]
    fn select_driver_for_tsv_uses_csv_driver() {
        let d = select_driver(&Uri::from_path("/tmp/sample.tsv"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "csv");
    }

    #[test]
    fn select_driver_for_geojson() {
        let d = select_driver(&Uri::from_path("/tmp/sample.geojson"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "geojson");
    }

    #[test]
    fn select_driver_for_geojsonl() {
        let d = select_driver(&Uri::from_path("/tmp/sample.geojsonl"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "geojson");
    }

    #[test]
    fn select_driver_for_ndjson() {
        let d = select_driver(&Uri::from_path("/tmp/sample.ndjson"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "geojson");
    }

    #[test]
    fn select_driver_for_jsonl() {
        let d = select_driver(&Uri::from_path("/tmp/sample.jsonl"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "geojson");
    }

    #[test]
    fn select_driver_for_gpkg() {
        let d = select_driver(&Uri::from_path("/tmp/sample.gpkg"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "gpkg");
    }

    #[test]
    fn select_driver_for_fgb() {
        let d = select_driver(&Uri::from_path("/tmp/sample.fgb"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "fgb");
    }

    #[test]
    fn select_driver_for_pg_url() {
        let d = select_driver(&Uri::from_path("pg://user:pass@localhost/db?table=t"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "postgis");
    }

    #[test]
    fn select_driver_for_postgresql_url_normalizes_to_pg() {
        let d = select_driver(&Uri::from_path("postgresql://h/db?table=t"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "postgis");
    }

    #[test]
    fn select_driver_for_mssql_url() {
        let d = select_driver(&Uri::from_path("mssql://sa:pass@localhost/db?table=t"));
        assert!(d.is_some());
        assert_eq!(d.unwrap().name(), "sqlserver");
    }

    #[test]
    fn all_drivers_sorted_by_name() {
        let names: Vec<&str> = all_drivers().iter().map(|d| d.name()).collect();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        assert_eq!(names, sorted, "all_drivers() must be sorted by name");
    }
}
