//! CLI から見える Driver の静的レジストリ。
//!
//! 各 driver はステートレスな unit struct なので `static` インスタンスを共有する。
//! `Box<dyn Driver>` を毎回 alloc していた v0.1 初期実装の改善。
//! `inventory` 等のグローバル登録機構は v0.2 で導入予定。

use shpx_core::{Driver, Error, Uri};

static SHP_DRIVER: shpx_driver_shp::ShpDriver = shpx_driver_shp::ShpDriver;
static PARQUET_DRIVER: shpx_driver_parquet::ParquetDriver = shpx_driver_parquet::ParquetDriver;

static ALL_DRIVERS: [&'static dyn Driver; 2] = [&SHP_DRIVER, &PARQUET_DRIVER];

/// ビルド時に組み込まれている全 Driver を返す。
#[must_use]
pub fn all_drivers() -> &'static [&'static dyn Driver] {
    &ALL_DRIVERS
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
        Error::Format(format!(
            "{}: no extension; cannot infer driver",
            uri.path()
        ))
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
}
