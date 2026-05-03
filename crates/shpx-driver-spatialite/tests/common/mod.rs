//! 統合テスト共有 helper。`tests/common/mod.rs` 配置にすると `cargo test` が
//! 別 test binary として扱わず、`mod common;` 宣言で各 test ファイルから読み込める。
//!
//! 環境変数 `SHPX_TEST_SPATIALITE` が未設定 (もしくは `0` / 空文字列) のときは
//! `skip_if_not_enabled() == true` でテストを skip する。

#![allow(dead_code)] // 各 test binary は一部しか使わないため、未使用 helper の warning を抑える。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_schema::{DataType, Field, Schema, SchemaRef};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, WriteOpts,
};

/// `SHPX_TEST_SPATIALITE` env が未設定なら eprintln + return で skip。
pub fn skip_if_not_enabled() -> bool {
    match std::env::var("SHPX_TEST_SPATIALITE") {
        Ok(v) if !v.is_empty() && v != "0" => false,
        _ => {
            eprintln!(
                "SHPX_TEST_SPATIALITE not set; skipping (install mod_spatialite + set the env to run)"
            );
            true
        }
    }
}

pub fn schema_with_geom(extras: Vec<Field>, gt: GeometryType, crs: Option<Crs>) -> SchemaRef {
    let mut fields = extras;
    let mut g = Field::new("geom", DataType::Binary, true);
    let mut m = HashMap::new();
    m.insert(
        GEOMETRY_META_KEY.to_string(),
        GeometryMeta::wkb(gt, crs).to_json().unwrap(),
    );
    g.set_metadata(m);
    fields.push(g);
    Arc::new(Schema::new(fields))
}

pub fn default_write_opts() -> WriteOpts {
    WriteOpts {
        overwrite: true,
        ..Default::default()
    }
}
