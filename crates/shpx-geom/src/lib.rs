//! shpx-geom — ジオメトリ表現・WKB encode/decode・CRS ユーティリティを提供する。
//!
//! v0.1 では PROJ 非依存。CRS は EPSG コードのみで保持・伝搬する。

pub mod epsg_wkt1;
pub mod geom_walk;
pub mod gpkg_blob;
pub mod projjson;
pub mod reproject;
pub mod wkb;
pub mod wkt;
pub mod wkt1_prj;

pub use epsg_wkt1::epsg_to_wkt1;
pub use geom_walk::for_each_coord_mut;
pub use projjson::minimal_for_epsg;
pub use reproject::{parse_target_crs, Reprojector};
pub use wkb::{decode, encode, Geom};
pub use wkt1_prj::extract_epsg;
