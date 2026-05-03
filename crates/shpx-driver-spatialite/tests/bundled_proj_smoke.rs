//! v0.6 cycle 3 完了基準の smoke test。`bundled-spatialite` feature 有効ビルドで
//! PROJ 依存関数 (`Transform`) が動くことを確認する。
//!
//! `bundled-spatialite` 経路でしか意味がないため `#![cfg(feature = "bundled-spatialite")]`
//! でファイル全体を gate する (システム libspatialite を見ている dynamic ロード経路では
//! 「PROJ が bundled で同梱できているか」の検証にならない)。
//!
//! env-gated は不要: feature ON でビルドが通った時点で libspatialite + libproj が
//! 同梱されており、test 単体で自走できる。

#![cfg(feature = "bundled-spatialite")]

use shpx_driver_spatialite::conn::open_write_new;
use tempfile::tempdir;

/// 東京駅付近 (経度 139.767, 緯度 35.681) を Web Mercator (EPSG:3857) に変換した時の
/// 概算座標 (m 単位)。誤差は 1km 以内に収まれば PROJ datum DB が解決できている十分な証拠。
const TOKYO_MERCATOR_X_MIN: f64 = 15_550_000.0;
const TOKYO_MERCATOR_X_MAX: f64 = 15_560_000.0;
const TOKYO_MERCATOR_Y_MIN: f64 = 4_250_000.0;
const TOKYO_MERCATOR_Y_MAX: f64 = 4_260_000.0;

/// EPSG:3857 (WGS 84 / Pseudo-Mercator) の最小 proj4text。`InitSpatialMetadata(1)` の
/// FastInit は WGS84 系のみ seed するため、Web Mercator は明示的に `spatial_ref_sys`
/// に INSERT してから Transform を呼ぶ必要がある。libspatialite は proj4text を
/// `proj_create` 経由で libproj に渡し、bundled libproj 9.x が proj-string を解釈する。
const EPSG_3857_PROJ4: &str = "+proj=merc +a=6378137 +b=6378137 +lat_ts=0 +lon_0=0 +x_0=0 +y_0=0 +k=1 +units=m +nadgrids=@null +wktext +no_defs";

#[test]
fn transform_4326_to_3857_uses_bundled_proj() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("transform.sqlite");

    let conn = open_write_new(&path).expect("open_write_new (bundled libspatialite + PROJ)");

    // FastInit (InitSpatialMetadata(1)) が seed しない EPSG:3857 を最小行で登録する。
    // srtext は NULL でも libspatialite は proj4text 経由で解決できる。
    // FastInit が placeholder 行 (auth_name=NULL) を残している可能性に備えて REPLACE を使う。
    conn.execute(
        "INSERT OR REPLACE INTO spatial_ref_sys \
         (srid, auth_name, auth_srid, ref_sys_name, proj4text, srtext) \
         VALUES (3857, 'EPSG', 3857, 'WGS 84 / Pseudo-Mercator', ?1, NULL)",
        [EPSG_3857_PROJ4],
    )
    .expect("INSERT spatial_ref_sys EPSG:3857");

    // Sanity check: 行が確かに入っていること。
    let row_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM spatial_ref_sys WHERE srid = 3857 AND auth_name IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .expect("count spatial_ref_sys row");
    assert_eq!(row_count, 1, "EPSG:3857 row must be present");

    // GeomFromText で WGS84 (4326) Point を作り、Transform で Web Mercator (3857) に投影、
    // AsBinary で WKB にして取り出す。PROJ が bundled でリンクされていない場合は
    // Transform が NULL を返す (UDF 未登録 or PROJ_NEW 分岐の `#error` 等)。
    let buf: Option<Vec<u8>> = conn
        .query_row(
            "SELECT AsBinary(Transform(GeomFromText('POINT(139.767 35.681)', 4326), 3857))",
            [],
            |row| row.get(0),
        )
        .expect("Transform query should succeed");

    let wkb = buf.expect("Transform should not return NULL when PROJ is bundled");
    assert_eq!(wkb.len(), 21, "Point WKB must be 21 bytes, got {}", wkb.len());
    assert_eq!(wkb[0], 0x01, "result must be little-endian WKB");
    assert_eq!(
        u32::from_le_bytes([wkb[1], wkb[2], wkb[3], wkb[4]]),
        1,
        "Transform of a Point should yield WKB type 1 (Point)"
    );

    let x = f64::from_le_bytes(wkb[5..13].try_into().unwrap());
    let y = f64::from_le_bytes(wkb[13..21].try_into().unwrap());
    assert!(
        (TOKYO_MERCATOR_X_MIN..=TOKYO_MERCATOR_X_MAX).contains(&x),
        "Mercator X for Tokyo should be ~15,557,000 m, got {x}",
    );
    assert!(
        (TOKYO_MERCATOR_Y_MIN..=TOKYO_MERCATOR_Y_MAX).contains(&y),
        "Mercator Y for Tokyo should be ~4,253,000 m, got {y}",
    );
}
