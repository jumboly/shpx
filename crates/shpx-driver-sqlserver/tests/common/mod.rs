//! 統合テスト共有 helper。`tests/common/mod.rs` 配置にすると `cargo test` が
//! 別 test binary として扱わず、`mod common;` 宣言で各 test ファイルから読み込める。
//!
//! 環境変数 `SHPX_TEST_SQLSERVER_URL` が未設定なら全テストは `mssql_url() == None` でスキップする。

#![allow(dead_code)] // 各 test binary は一部しか使わないため、未使用 helper の warning を抑える。

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_schema::{DataType, Field, Schema, SchemaRef};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Uri, WriteOpts,
};
use shpx_driver_sqlserver::conn;

pub fn mssql_url() -> Option<String> {
    std::env::var("SHPX_TEST_SQLSERVER_URL")
        .ok()
        .filter(|s| !s.is_empty())
}

pub fn unique_table(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{}_{nanos}", std::process::id())
}

pub fn uri_with_table(base_url: &str, table: &str) -> Uri {
    let sep = if base_url.contains('?') { '&' } else { '?' };
    Uri::from_path(format!("{base_url}{sep}table={table}"))
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

pub fn write_opts() -> WriteOpts {
    WriteOpts {
        overwrite: true,
        ..Default::default()
    }
}

/// 失敗しても無視する best-effort なテーブル削除。テスト終了時に呼ぶ。
pub fn cleanup(base_url: &str, table: &str) {
    let Ok(mut client) = conn::connect(base_url) else {
        return;
    };
    let _ = conn::simple_query(
        &mut client,
        format!("IF OBJECT_ID(N'[dbo].[{table}]', 'U') IS NOT NULL DROP TABLE [dbo].[{table}]"),
    );
}
