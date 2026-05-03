//! v0.6 cycle 2 完了基準の smoke test。`bundled-spatialite` feature 有効ビルドで
//! GEOS 依存関数 (`ST_Buffer`) が動くことを確認する。
//!
//! `bundled-spatialite` 経路でしか意味がないため `#![cfg(feature = "bundled-spatialite")]`
//! でファイル全体を gate する (システム libspatialite を見ている dynamic ロード経路では
//! 「GEOS が bundled で同梱できているか」の検証にならない)。
//!
//! env-gated は不要: feature ON でビルドが通った時点で libspatialite + libgeos が
//! 同梱されており、test 単体で自走できる。

#![cfg(feature = "bundled-spatialite")]

use shpx_driver_spatialite::conn::open_write_new;
use tempfile::tempdir;

/// POINT(0 0) の WKB (little endian, SRID なし)。
/// byte order (1) | type=1 (4) | x=0.0 (8) | y=0.0 (8) = 21 bytes.
const POINT_0_0_WKB: [u8; 21] = [
    0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00,
];

#[test]
fn st_buffer_returns_non_null_blob_under_bundled_geos() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("buf.sqlite");

    let conn = open_write_new(&path).expect("open_write_new (bundled libspatialite + GEOS)");

    // SpatiaLite の GeomFromWKB(blob, srid) で WKB を SpatiaLite blob に変換し、
    // ST_Buffer で 0.1 度バッファした結果を AsBinary で WKB に戻す。
    // GEOS が bundled でリンクされていないと ST_Buffer は NULL を返す (UDF 未登録)
    // ため、非 NULL かつ 21 bytes (Point) より大きい WKB を assert することで
    // GEOS が呼び出された証拠とする。
    let buf: Option<Vec<u8>> = conn
        .query_row(
            "SELECT AsBinary(ST_Buffer(GeomFromWKB(?1, 4326), 0.1))",
            [&POINT_0_0_WKB[..]],
            |row| row.get(0),
        )
        .expect("ST_Buffer query");

    let wkb = buf.expect("ST_Buffer should not return NULL when GEOS is bundled");
    assert!(
        wkb.len() > POINT_0_0_WKB.len(),
        "ST_Buffer result should be a Polygon (much larger than the input Point WKB), got {} bytes",
        wkb.len()
    );
    // Polygon WKB の先頭 5 バイト = byte order (1) + type=3 (4 LE).
    assert_eq!(wkb[0], 0x01, "result must be little-endian WKB");
    assert_eq!(
        u32::from_le_bytes([wkb[1], wkb[2], wkb[3], wkb[4]]),
        3,
        "ST_Buffer of a Point should yield WKB type 3 (Polygon)"
    );
}
