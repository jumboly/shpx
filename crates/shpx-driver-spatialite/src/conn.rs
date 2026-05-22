//! `rusqlite::Connection` の構築 + `mod_spatialite` 動的ロード + メタデータ初期化。
//!
//! reader / writer 双方の open 経路で共有する。
//! SpatiaLite を有効化するには SQLite に `mod_spatialite` 共有ライブラリをロードする
//! 必要があるため、shpx は接続直後に必ず本モジュール経由で extension をロードする。
//!
//! libspatialite は bundle しない（[ADR-0006]）。ユーザーが用意した `mod_spatialite` を
//! `load_extension` で runtime ロードするのが唯一の経路:
//! 1. 環境変数 `SHPX_SPATIALITE_PATH` で指定された絶対パス / 相対パスを `load_extension`。
//! 2. 既定: `mod_spatialite` を SQLite に渡し、OS のライブラリ検索パスから dlopen。
//!
//! [ADR-0006]: ../../../docs/adr/0006-spatialite-system-dependency-not-bundled.md

use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OpenFlags};
use shpx_core::Result;

use crate::meta::SQL_GEOMETRY_COLUMNS_EXISTS;
use crate::util::{driver_err, driver_msg};

/// Why: libspatialite は extension load (`spatialite_init_ex`) と
/// `InitSpatialMetadata()` の双方で global PROJ / GEOS context や allocator caches を
/// 変更するが、公式 docs はスレッドセーフを保証していない。`cargo test` の並列実行で
/// 複数 connection が同時に load + init すると glibc malloc が `double free or
/// corruption` で SIGABRT する flaky 失敗を CI で観測したため、process 単一の Mutex で
/// load + init を serialize する。open_read / open_write_new の双方で共通利用する。
/// 実害は init 直後の数 ms ロックのみで、SQL 実行中は Mutex を握らない。
static LIBSPATIALITE_INIT_LOCK: Mutex<()> = Mutex::new(());

fn lock_libspatialite_init() -> MutexGuard<'static, ()> {
    LIBSPATIALITE_INIT_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

/// 環境変数: `mod_spatialite` 共有ライブラリのパス上書き。
pub const ENV_SPATIALITE_PATH: &str = "SHPX_SPATIALITE_PATH";

/// 既存ファイルを read-only で開き、mod_spatialite をロードする。
pub fn open_read(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags).map_err(|e| driver_err(&e))?;
    let _guard = lock_libspatialite_init();
    load_mod_spatialite(&conn)?;
    verify_geometry_columns(&conn)?;
    Ok(conn)
}

/// 新規 SpatiaLite ファイル（または既存ファイル）を read-write で開き、mod_spatialite をロード後、
/// `InitSpatialMetadata(1)` を idempotent に発行する。FastInit (=1) で WGS84 系のみ seed する
/// （全 EPSG seed は数秒かかり、空 DB を頻繁に作る用途では重い）。
pub fn open_write_new(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| driver_err(&e))?;
    let _guard = lock_libspatialite_init();
    load_mod_spatialite(&conn)?;
    init_spatial_metadata(&conn)?;
    Ok(conn)
}

/// `geometry_columns` テーブルが存在することを検証する。
/// SpatiaLite 化されていない素の SQLite ファイルを reader が開いた場合に明示的にエラーにする。
pub fn verify_geometry_columns(conn: &Connection) -> Result<()> {
    let n: i64 = conn
        .query_row(SQL_GEOMETRY_COLUMNS_EXISTS, [], |row| row.get(0))
        .map_err(|e| driver_err(&e))?;
    if n == 0 {
        return Err(driver_msg(
            "not a SpatiaLite database: `geometry_columns` table is missing",
        ));
    }
    Ok(())
}

fn load_mod_spatialite(conn: &Connection) -> Result<()> {
    let path = std::env::var(ENV_SPATIALITE_PATH).unwrap_or_else(|_| "mod_spatialite".to_string());

    // SAFETY: 開発者が指定する extension path のみをロードする。実行中に他クエリは
    // 並走しない初期化フェーズで `LoadExtensionGuard` を取り、終了時に enable を切る。
    // 本関数の呼び出し前後で SQLite ハンドルに対する untrusted SQL は流れない。
    #[allow(unsafe_code)]
    unsafe {
        let _guard = rusqlite::LoadExtensionGuard::new(conn).map_err(|e| driver_err(&e))?;
        conn.load_extension(&path, None).map_err(|e| {
            driver_msg(format!(
                "failed to load mod_spatialite at `{path}`: {e}. \
                 Set {ENV_SPATIALITE_PATH} or install libsqlite3-mod-spatialite \
                 (Ubuntu) / libspatialite (macOS brew)."
            ))
        })?;
    }
    Ok(())
}

fn init_spatial_metadata(conn: &Connection) -> Result<()> {
    // FastInit (=1) で WGS84 系のみ seed。全 EPSG seed (`InitSpatialMetadata(0)`) は
    // 数秒かかり、空 DB を頻繁に作る用途では重い。EPSG 必要時は writer 側で
    // best-effort INSERT する (cycle 2)。
    conn.query_row("SELECT InitSpatialMetadata(1)", [], |_| Ok(()))
        .map_err(|e| driver_msg(format!("InitSpatialMetadata(1) failed: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// `SHPX_TEST_SPATIALITE` env 未設定時はテストを skip する。
    /// `mod_spatialite` がインストールされていない環境で本ファイルの test を回さないため。
    fn skip_if_not_enabled() -> bool {
        match std::env::var("SHPX_TEST_SPATIALITE") {
            Ok(v) if !v.is_empty() && v != "0" => false,
            _ => {
                eprintln!("SHPX_TEST_SPATIALITE not set; skipping (install mod_spatialite to run)");
                true
            }
        }
    }

    #[test]
    fn open_write_new_loads_extension_and_inits_metadata() {
        if skip_if_not_enabled() {
            return;
        }
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.sqlite");
        let conn = open_write_new(&path).unwrap();
        // `geometry_columns` が InitSpatialMetadata で生成されている。
        verify_geometry_columns(&conn).unwrap();
    }

    #[test]
    fn open_read_rejects_plain_sqlite() {
        if skip_if_not_enabled() {
            return;
        }
        let dir = tempdir().unwrap();
        let path = dir.path().join("plain.sqlite");
        // SpatiaLite 化していない素の SQLite を作る。
        Connection::open(&path).unwrap().close().unwrap();
        // open_read で mod_spatialite はロードできるが、verify_geometry_columns が
        // missing でエラーになる。
        let err = open_read(&path).unwrap_err();
        assert!(matches!(err, shpx_core::Error::Driver { .. }));
    }
}
