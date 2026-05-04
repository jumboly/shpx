//! Streaming reader の Drop 動作テスト。
//!
//! 目的:
//! - 中途で Reader を drop した時、background OS thread が clean に終了することを
//!   `tx.send` Err 経由で検出できる構造であることを確認する。
//! - drop 直後にテーブル DROP できる (= worker が SELECT を解放している) ことで
//!   間接的に worker 終了を検証する。
//!
//! `SHPX_TEST_PG_URL` 未設定なら skip。

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use arrow_array::{
    builder::{BinaryBuilder, Int32Builder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field};
use shpx_core::{schema::GeometryType, Crs, Driver, ReadOpts};
use shpx_driver_postgis::PostgisDriver;
use shpx_geom::wkb::{self, Geom};

mod common;
use common::{cleanup, pg_url, schema_with_geom, unique_table, uri_with_table, write_opts};

/// 1000 行を書いて Reader を開き、最初の 1 batch だけ取り出して drop する。
/// drop 後に DROP TABLE が成功することを確認する (worker が SELECT を解放している証左)。
#[test]
fn reader_drop_releases_select_promptly() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_cancel");
    let uri = uri_with_table(&url, &table);

    // 約 1000 行を 1 batch で書き込む。
    let n: usize = 1000;
    let schema = schema_with_geom(
        vec![Field::new("id", DataType::Int32, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut id_b = Int32Builder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 0..n {
        id_b.append_value(i32::try_from(i).unwrap());
        geom_b.append_value(
            wkb::encode(&Geom::Point(f64::from(i32::try_from(i).unwrap()), 0.0)).unwrap(),
        );
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(id_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    // Reader を開いて 1 batch だけ取り、明示 drop。
    {
        let mut r = driver
            .open_read(&uri, &ReadOpts::default())
            .expect("open_read");
        let mut iter = r.batches();
        // 65536 行 batch で 1 batch でも 1000 行全部入るが、`next()` を 1 回呼ぶだけにとどめる。
        let _first = iter.next();
        // iter / r を drop する。background worker への send が Err になり worker が return する。
    }

    // worker 終了を待つ短い grace period (channel disconnect の伝播)。
    thread::sleep(Duration::from_millis(200));

    // worker が SELECT を解放していれば DROP TABLE は即時成功する。worker が SELECT
    // を抱えたままだと PostgreSQL は ACCESS EXCLUSIVE LOCK 取得待ちでブロックする。
    cleanup(&url, &table);
}

/// 全 batch を最後まで pull してから Reader を drop した場合も問題なくクリーンアップできる。
/// 通常の roundtrip テストでも実質これを確認しているが、明示テストとして 1 件残す。
#[test]
fn reader_full_consume_drops_cleanly() {
    let Some(url) = pg_url() else {
        eprintln!("SHPX_TEST_PG_URL unset; skipping integration test");
        return;
    };
    let table = unique_table("shpx_full");
    let uri = uri_with_table(&url, &table);

    let n: usize = 500;
    let schema = schema_with_geom(
        vec![Field::new("id", DataType::Int32, true)],
        GeometryType::Point,
        Some(Crs::from_epsg(4326)),
    );

    let mut id_b = Int32Builder::new();
    let mut geom_b = BinaryBuilder::new();
    for i in 0..n {
        id_b.append_value(i32::try_from(i).unwrap());
        geom_b.append_value(
            wkb::encode(&Geom::Point(f64::from(i32::try_from(i).unwrap()), 0.0)).unwrap(),
        );
    }
    let cols: Vec<ArrayRef> = vec![Arc::new(id_b.finish()), Arc::new(geom_b.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = PostgisDriver::new();
    let mut w = driver
        .open_write(
            &uri,
            schema.clone(),
            Some(Crs::from_epsg(4326)),
            &write_opts(),
        )
        .expect("open_write");
    w.write_batch(&batch).expect("write_batch");
    w.finish().expect("finish");

    let mut r = driver
        .open_read(&uri, &ReadOpts::default())
        .expect("open_read");
    let total: usize = r.batches().map(|b| b.expect("batch ok").num_rows()).sum();
    assert_eq!(total, n);

    cleanup(&url, &table);
}
