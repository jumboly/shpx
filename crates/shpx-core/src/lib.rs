//! shpx-core — shpx の中核トレイトと共通型を定義するクレート。
//!
//! このクレートは具体的なフォーマット実装を持たない。各 driver クレートが
//! ここで定義した [`Driver`] / [`LayerReader`] / [`LayerWriter`] トレイトを
//! 実装し、CLI は driver を組み合わせて変換パイプラインを構成する。
//!
//! 設計の詳細は `docs/DESIGN.md` を参照。

pub mod bench_util;
pub mod capabilities;
pub mod crs;
pub mod driver;
pub mod error;
pub mod opts;
pub mod schema;
pub mod uri;

pub use capabilities::{Capabilities, StringEncoding};
pub use crs::{Crs, WktFlavor};
pub use driver::{BulkLoadWriter, Driver, DriverRegistration, LayerReader, LayerWriter};
pub use error::{Error, Result};
pub use opts::{CreateIndex, CreateTable, OnLoss, ReadOpts, WriteOpts};
pub use schema::{Edges, GeometryEncoding, GeometryMeta, GeometryType, GEOMETRY_META_KEY};
pub use uri::Uri;

// `inventory` を再エクスポートし、driver crate 側は `shpx_core::inventory::submit!`
// だけで登録できるようにする。各 driver から `inventory` を直接依存させない。
pub use inventory;
