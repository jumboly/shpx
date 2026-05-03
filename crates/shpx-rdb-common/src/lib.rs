//! shpx-rdb-common — RDB 系 driver で共有する純粋ヘルパー群。
//!
//! PostGIS / SQL Server の 2 driver を v0.4 で揃えた段階で `options.rs` / `util.rs` に
//! 同型コードが目立つようになったため、driver crate を跨いで再利用できる「DB 方言を
//! 含まない」関数だけを集めた crate。次の RDB driver (MySQL 等) を追加する際の
//! boilerplate を削減することを主目的とする。
//!
//! 設計方針:
//! - tokio-postgres / tiberius といった client crate には依存しない（async runtime 中立）。
//! - SQL 方言（識別子クオート、文字列リテラル、catalog クエリ）は driver 側に残す。
//! - tracing target / driver_name 等は呼び出し側から `&'static str` で受け取り、
//!   どの driver から呼ばれても同じ振る舞いが得られるようにする。

pub mod arrow;
pub mod crs;
pub mod on_loss;
pub mod opts;
pub mod streaming;
pub mod table;
pub mod uri;

pub use arrow::primitive;
pub use crs::{merge_crs, resolve_epsg_srid};
pub use on_loss::apply_on_loss;
pub use opts::validate_overwrite_compat;
pub use table::{resolve_table_name, split_qualified};
pub use uri::{percent_decode, query_get, query_pairs};
