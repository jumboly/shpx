//! `rusqlite::Connection` の構築と PRAGMA 設定をまとめる。
//!
//! reader / writer 双方の open 経路で共有するため、ここで「shpx の標準的な GPKG 接続」を定義する。

use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use shpx_core::Result;

use crate::meta::{APPLICATION_ID, USER_VERSION};
use crate::util::driver_err;

/// 既存ファイルを read-only で開く。
///
/// `SQLITE_OPEN_READ_ONLY | SQLITE_OPEN_NO_MUTEX` で開く。SQLite はファイル単位ロックを
/// 持つため、shared cache は不要。
pub fn open_read(path: &Path) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    Connection::open_with_flags(path, flags).map_err(|e| driver_err(&e))
}

/// 新規 GeoPackage ファイルを作成する。`open` 前に呼び出し側が既存ファイルを削除する想定。
///
/// 設定する PRAGMA:
/// - `application_id` / `user_version` — GPKG ファイルマジックとして必須
/// - `foreign_keys = ON` — gpkg_contents.srs_id の外部キーを実効化
/// - `journal_mode = WAL` — 大量 INSERT 時の性能。crash safe で fsync 回数を減らせる
/// - `synchronous = NORMAL` — WAL モードと組み合わせると一般的に安全な設定
pub fn open_write_new(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "application_id", APPLICATION_ID)
        .map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "user_version", USER_VERSION)
        .map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|e| driver_err(&e))?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| driver_err(&e))?;
    Ok(conn)
}

/// `application_id` PRAGMA を読み出して GPKG であることを検証する。
///
/// 仕様上 GPKG 1.0/1.1 は `'GP10'`/`'GP11'` を使うが、shpx は 1.2+ (`'GPKG'`) のみ受け付ける。
/// 後方互換が必要になったら本関数を緩めるが、無音で誤判定するよりは明示エラーを返す。
pub fn verify_application_id(conn: &Connection) -> Result<()> {
    let id: i32 = conn
        .pragma_query_value(None, "application_id", |row| row.get(0))
        .map_err(|e| driver_err(&e))?;
    if id == APPLICATION_ID {
        Ok(())
    } else {
        Err(crate::util::driver_msg(format!(
            "not a GeoPackage: application_id = {id:#010x} (expected {APPLICATION_ID:#010x})"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn open_write_new_sets_application_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.gpkg");
        {
            let conn = open_write_new(&path).unwrap();
            // ハンドルが drop される前に PRAGMA が永続化されるよう明示的に閉じる。
            conn.close().unwrap();
        }
        // 別接続で確認。
        let conn = Connection::open(&path).unwrap();
        let id: i32 = conn
            .pragma_query_value(None, "application_id", |row| row.get(0))
            .unwrap();
        assert_eq!(id, APPLICATION_ID);
        let v: i32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(v, USER_VERSION);
    }

    #[test]
    fn verify_application_id_rejects_plain_sqlite() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plain.sqlite");
        // PRAGMA を設定せず素の SQLite ファイルを作る。
        Connection::open(&path).unwrap().close().unwrap();
        let conn = open_read(&path).unwrap();
        let err = verify_application_id(&conn).unwrap_err();
        assert!(matches!(err, shpx_core::Error::Driver { .. }));
    }
}
