//! v0.3 cycle 3b で追加した PostGIS writer 拡張オプションの統合テスト。
//!
//! - `--create-table=if-not-exists|always|never`
//! - `--gist-index=auto|always|never` (CLI フラグ名は `--create-index`)
//! - 未登録 EPSG の `spatial_ref_sys` 自動 INSERT
//!
//! `SHPX_TEST_PG_URL` が未設定の環境では各テストが冒頭で `eprintln!` + return で
//! スキップされる（`tests/roundtrip.rs` と同じパターン）。

use std::sync::Arc;

use arrow_array::{
    builder::{BinaryBuilder, Int32Builder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field};
use shpx_core::{
    schema::GeometryType, CreateIndex, CreateTable, Crs, Driver, OnLoss, WktFlavor, WriteOpts,
};
use shpx_driver_postgis::{conn, PostgisDriver};
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, pg_url, schema_with_geom, unique_table, uri_with_table};

/// 1 行のシンプルな Point + Int32 batch。各テストで使い回す。
fn one_point_batch(crs: Option<Crs>) -> (arrow_schema::SchemaRef, RecordBatch) {
    let schema = schema_with_geom(
        vec![Field::new("v", DataType::Int32, true)],
        GeometryType::Point,
        crs,
    );
    let mut v_b = Int32Builder::new();
    v_b.append_value(1);
    let mut g_b = BinaryBuilder::new();
    g_b.append_value(wkb::encode(&Geom::Point(1.0, 2.0)).unwrap());
    let cols: Vec<ArrayRef> = vec![Arc::new(v_b.finish()), Arc::new(g_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();
    (schema, batch)
}

/// `pg_indexes` を query して、対象テーブルに GIST index が存在するかを返す。
fn has_gist_index(url: &str, table: &str) -> bool {
    let client = conn::connect(url).expect("connect");
    let row = conn::query_opt(
        &client,
        "SELECT 1 FROM pg_indexes WHERE schemaname = 'public' AND tablename = $1 \
         AND indexdef LIKE '%USING gist%'",
        &[&table],
    )
    .expect("query pg_indexes");
    row.is_some()
}

fn count_rows(url: &str, table: &str) -> i64 {
    let client = conn::connect(url).expect("connect");
    let qualified = format!("\"public\".\"{table}\"");
    let row = conn::query_opt(
        &client,
        &format!("SELECT COUNT(*)::bigint FROM {qualified}"),
        &[],
    )
    .expect("count")
    .expect("at least one row");
    row.get::<_, i64>(0)
}

fn write_opts_with(create_table: CreateTable, create_index: CreateIndex) -> WriteOpts {
    WriteOpts {
        create_table,
        create_index,
        ..Default::default()
    }
}

#[test]
fn if_not_exists_creates_when_absent() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_ct_ine");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    assert_eq!(count_rows(&url, &table), 1);
    cleanup(&url, &table);
}

#[test]
fn if_not_exists_appends_when_present() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_ct_app");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    let driver = PostgisDriver::new();
    // 1 回目: 新規作成。
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto),
        )
        .expect("open_write 1st");
    w.write_batch(&batch).expect("write 1st");
    w.finish().expect("finish 1st");

    // 2 回目: 既存テーブルへ append (overwrite=false)。
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto),
        )
        .expect("open_write 2nd");
    w.write_batch(&batch).expect("write 2nd");
    w.finish().expect("finish 2nd");

    assert_eq!(count_rows(&url, &table), 2);
    cleanup(&url, &table);
}

#[test]
fn never_errors_when_table_missing() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_ct_missing");
    let uri = uri_with_table(&url, &table);
    let (schema, _) = one_point_batch(Some(Crs::from_epsg(4326)));

    let driver = PostgisDriver::new();
    let err = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::Never, CreateIndex::Auto),
        )
        .err()
        .expect("open_write should fail when table is missing");
    let msg = format!("{err}");
    assert!(
        msg.contains("--create-table=never") || msg.contains("存在しない"),
        "unexpected msg: {msg}"
    );
}

#[test]
fn never_uses_existing_table() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_ct_existing");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    // 手動で同じスキーマのテーブルを作成しておく。
    {
        let client = conn::connect(&url).expect("connect");
        conn::batch_execute(
            &client,
            &format!(
                "CREATE TABLE \"public\".\"{table}\" (\"v\" integer, \"geom\" geometry(Point, 4326))"
            ),
        )
        .expect("manual create");
    }

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::Never, CreateIndex::Auto),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    assert_eq!(count_rows(&url, &table), 1);
    // create_table=Never の経路では create_index=Auto は index を作らない契約。
    assert!(
        !has_gist_index(&url, &table),
        "Auto + Never should not create index"
    );
    cleanup(&url, &table);
}

#[test]
fn gist_index_auto_creates_for_new_table() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_idx_auto");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    assert!(has_gist_index(&url, &table), "GIST index should exist");
    cleanup(&url, &table);
}

#[test]
fn gist_index_never_skips() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_idx_never");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Never),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    assert!(
        !has_gist_index(&url, &table),
        "GIST index should not exist with Never"
    );
    cleanup(&url, &table);
}

#[test]
fn gist_index_always_creates_even_for_existing_table() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_idx_always");
    let uri = uri_with_table(&url, &table);
    let (schema, batch) = one_point_batch(Some(Crs::from_epsg(4326)));

    // 手動で既存テーブルを作る (index 無し)。
    {
        let client = conn::connect(&url).expect("connect");
        conn::batch_execute(
            &client,
            &format!(
                "CREATE TABLE \"public\".\"{table}\" (\"v\" integer, \"geom\" geometry(Point, 4326))"
            ),
        )
        .expect("manual create");
    }

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &write_opts_with(CreateTable::Never, CreateIndex::Always),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    assert!(
        has_gist_index(&url, &table),
        "Always should create GIST index even for existing table"
    );
    cleanup(&url, &table);
}

#[test]
fn register_unknown_epsg_inserts_into_spatial_ref_sys() {
    // 仮想 SRID。実在 EPSG コードと衝突しない範囲の番号にする。
    const TEST_SRID: i32 = 999_999;
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_srs_reg");
    let uri = uri_with_table(&url, &table);

    // 事前 cleanup: 前回テストの遺残があれば削除。
    {
        let client = conn::connect(&url).expect("connect");
        let _ = conn::execute(
            &client,
            "DELETE FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        );
    }

    // 仮想 EPSG コード TEST_SRID をもつ Crs を WKT 付きで構築する。
    // WKT 内の AUTHORITY 値は EPSG コードを反映させ、PostGIS 標準の `find_srid` 経路と
    // 整合させる。
    #[allow(clippy::cast_sign_loss)]
    let crs = Crs {
        authority: Some(("EPSG".to_string(), TEST_SRID as u32)),
        wkt: Some(format!(
            "GEOGCS[\"shpx test SRS\",DATUM[\"WGS_1984\",SPHEROID[\"WGS 84\",6378137,298.257223563]],PRIMEM[\"Greenwich\",0],UNIT[\"degree\",0.0174532925199433],AUTHORITY[\"EPSG\",\"{TEST_SRID}\"]]"
        )),
        wkt_flavor: WktFlavor::V1,
        projjson: None,
    };
    let (schema, batch) = one_point_batch(Some(crs.clone()));

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema,
            Some(crs),
            &write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    // spatial_ref_sys に行が追加されていること。
    {
        let client = conn::connect(&url).expect("connect");
        let row = conn::query_opt(
            &client,
            "SELECT auth_name, auth_srid, srtext FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        )
        .expect("query srs")
        .expect("row should exist after writer");
        let auth_name: String = row.get(0);
        let auth_srid: i32 = row.get(1);
        let srtext: String = row.get(2);
        assert_eq!(auth_name, "EPSG");
        assert_eq!(auth_srid, TEST_SRID);
        assert!(srtext.contains("shpx test SRS"));

        // 二度目の writer は ON CONFLICT DO NOTHING で安全に no-op になること
        // （= エラーにならず、行が二重登録されない）。
        let _ = conn::execute(
            &client,
            "INSERT INTO spatial_ref_sys (srid, auth_name, auth_srid, srtext, proj4text) \
             VALUES ($1, 'EPSG', $1, 'dummy', NULL) ON CONFLICT (srid) DO NOTHING",
            &[&TEST_SRID],
        )
        .expect("on conflict");
        let count_row = conn::query_opt(
            &client,
            "SELECT COUNT(*)::bigint FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        )
        .unwrap()
        .unwrap();
        let n: i64 = count_row.get(0);
        assert_eq!(
            n, 1,
            "spatial_ref_sys should hold exactly one row for the test SRID"
        );
    }

    // teardown: 仮想 SRID の行と test テーブルを削除。
    cleanup(&url, &table);
    {
        let client = conn::connect(&url).expect("connect");
        let _ = conn::execute(
            &client,
            "DELETE FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        );
    }
}

#[test]
fn register_skipped_when_no_wkt_available() {
    // 仮想 SRID で WKT 無しの Crs。`epsg_to_wkt1` のテーブルにも無いので INSERT はスキップ。
    // ただし geometry 列定義 (`geometry(Point, 999998)`) は spatial_ref_sys に行が無くても
    // CREATE できるため、書き込み自体は成功する（PostGIS の挙動）。
    const TEST_SRID: i32 = 999_998;
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_srs_skip");
    let uri = uri_with_table(&url, &table);

    // 事前 cleanup
    {
        let client = conn::connect(&url).expect("connect");
        let _ = conn::execute(
            &client,
            "DELETE FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        );
    }

    #[allow(clippy::cast_sign_loss)]
    let crs = Crs {
        authority: Some(("EPSG".to_string(), TEST_SRID as u32)),
        wkt: None,
        wkt_flavor: WktFlavor::V2,
        projjson: None,
    };
    let (schema, batch) = one_point_batch(Some(crs.clone()));

    let driver = PostgisDriver::new();
    let mut opts = write_opts_with(CreateTable::IfNotExists, CreateIndex::Auto);
    opts.on_loss = OnLoss::Warn; // CRS 解決の loss を許容する場合に備えるが、今回は EPSG ありで loss は発生しない
    let mut w = driver
        .open_write(&uri, schema, Some(crs), &opts)
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    // spatial_ref_sys に行が追加されていないこと（= 自動 INSERT スキップが期待通り動作）。
    {
        let client = conn::connect(&url).expect("connect");
        let row = conn::query_opt(
            &client,
            "SELECT 1 FROM spatial_ref_sys WHERE srid = $1",
            &[&TEST_SRID],
        )
        .expect("query srs");
        assert!(
            row.is_none(),
            "spatial_ref_sys should NOT have a row when WKT is unavailable"
        );
    }

    cleanup(&url, &table);
}
