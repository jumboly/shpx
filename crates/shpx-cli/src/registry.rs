//! CLI から見える Driver の静的レジストリ。
//!
//! `inventory` 等のグローバル登録機構は v0.2 で導入予定。

use shpx_core::{Driver, Uri};

/// ビルド時に組み込まれている全 Driver を返す。
#[must_use]
pub fn all_drivers() -> Vec<Box<dyn Driver>> {
    vec![Box::new(shpx_driver_shp::ShpDriver::new())]
}

/// URI のスキーム (拡張子) から該当 Driver を解決する。
#[must_use]
#[allow(dead_code)]
pub fn select_driver(uri: &Uri) -> Option<Box<dyn Driver>> {
    let scheme = uri.scheme.as_str();
    if scheme.is_empty() {
        return None;
    }
    all_drivers()
        .into_iter()
        .find(|d| d.supported_schemes().contains(&scheme))
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
        let drivers = all_drivers();
        assert!(drivers.iter().any(|d| d.name() == "shp"));
    }
}
