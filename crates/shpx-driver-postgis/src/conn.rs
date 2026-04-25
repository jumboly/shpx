//! PostgreSQL 接続のラッパ。`tokio_postgres::Client` を構築し、同期 API で
//! query/execute を呼べるようにする。
//!
//! - URI から libpq 互換の接続文字列を作る（`?table=...` 等の shpx 独自パラメータは
//!   事前に切り出して libpq には渡さない。tokio-postgres は未知パラメータを
//!   `Connect param contained an invalid name` で拒否するため）
//! - `pg://` スキームは libpq の `postgresql://` に置換する
//! - `Connection` future は `tokio::spawn` で別 task に逃がす（接続切断時のみ tracing で吐く）

use std::str::FromStr;

use tokio_postgres::{Client, Config, NoTls};

use crate::runtime::runtime;
use crate::util::driver_err;

/// driver 独自の URI クエリパラメータ。これらは libpq に渡さず、driver 側で解釈する。
const RESERVED_KEYS: &[&str] = &["table"];

/// `pg://user:pass@host/db?table=...&sslmode=...` を libpq URI に変換し、`tokio_postgres::Client` を返す。
pub fn connect(url: &str) -> shpx_core::Result<Client> {
    let cleaned = strip_reserved_query_params(url);
    let libpq_url = pg_to_libpq(&cleaned);
    let cfg = Config::from_str(&libpq_url).map_err(|e| driver_err(&e))?;
    let rt = runtime()?;
    let (client, conn) = rt
        .block_on(cfg.connect(NoTls))
        .map_err(|e| driver_err(&e))?;
    // Connection future は別 task で動かす。エラーは tracing で吐く。
    rt.spawn(async move {
        if let Err(e) = conn.await {
            tracing::error!(target: "shpx::postgis", error = %e, "connection error");
        }
    });
    Ok(client)
}

/// `pg://...` を `postgresql://...` に置換する。`tokio_postgres::Config::from_str` は
/// `postgres://` / `postgresql://` のみ受理するので、shpx の正規 scheme `pg` を変換する。
fn pg_to_libpq(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("pg://") {
        format!("postgresql://{rest}")
    } else {
        url.to_string()
    }
}

/// URL から RESERVED_KEYS を除去した libpq 互換 URL を返す。
fn strip_reserved_query_params(url: &str) -> String {
    let Some((path, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let kept: Vec<&str> = query
        .split('&')
        .filter(|pair| {
            let key = pair.split_once('=').map_or(*pair, |(k, _)| k);
            !RESERVED_KEYS.iter().any(|r| key.eq_ignore_ascii_case(r))
        })
        .collect();
    if kept.is_empty() {
        path.to_string()
    } else {
        format!("{path}?{}", kept.join("&"))
    }
}

/// 同期ラッパ: `client.query(...)` を block_on する。
pub fn query(
    client: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> shpx_core::Result<Vec<tokio_postgres::Row>> {
    let rt = runtime()?;
    rt.block_on(client.query(sql, params))
        .map_err(|e| driver_err(&e))
}

/// 同期ラッパ: 0 または 1 件期待。
pub fn query_opt(
    client: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> shpx_core::Result<Option<tokio_postgres::Row>> {
    let rt = runtime()?;
    rt.block_on(client.query_opt(sql, params))
        .map_err(|e| driver_err(&e))
}

/// 同期ラッパ: `batch_execute`（複数文の SQL スクリプト）。
pub fn batch_execute(client: &Client, sql: &str) -> shpx_core::Result<()> {
    let rt = runtime()?;
    rt.block_on(client.batch_execute(sql))
        .map_err(|e| driver_err(&e))
}

/// 同期ラッパ: パラメータ化された 1 文の `execute`（影響行数を返す）。
/// `INSERT` / `UPDATE` / `DELETE` でユーザー由来の値を bind して安全に発行するために使う。
pub fn execute(
    client: &Client,
    sql: &str,
    params: &[&(dyn tokio_postgres::types::ToSql + Sync)],
) -> shpx_core::Result<u64> {
    let rt = runtime()?;
    rt.block_on(client.execute(sql, params))
        .map_err(|e| driver_err(&e))
}

/// 同期ラッパ: prepared statement を作って返す。connection-scoped なので
/// transaction 跨ぎで再利用できる（`Statement` は内部 Arc で Clone も安価）。
pub fn prepare(client: &Client, sql: &str) -> shpx_core::Result<tokio_postgres::Statement> {
    let rt = runtime()?;
    rt.block_on(client.prepare(sql)).map_err(|e| driver_err(&e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pg_to_libpq_replaces_scheme() {
        assert_eq!(
            pg_to_libpq("pg://u:p@h:5432/db?table=t"),
            "postgresql://u:p@h:5432/db?table=t"
        );
        assert_eq!(pg_to_libpq("postgresql://u@h/db"), "postgresql://u@h/db");
    }

    #[test]
    fn strip_reserved_keeps_other_params() {
        assert_eq!(
            strip_reserved_query_params("pg://h/db?sslmode=require&table=t&connect_timeout=5"),
            "pg://h/db?sslmode=require&connect_timeout=5"
        );
    }

    #[test]
    fn strip_reserved_drops_query_when_only_reserved() {
        assert_eq!(
            strip_reserved_query_params("pg://h/db?table=t"),
            "pg://h/db"
        );
    }

    #[test]
    fn strip_reserved_no_query_returns_input() {
        assert_eq!(strip_reserved_query_params("pg://h/db"), "pg://h/db");
    }

    #[test]
    fn strip_reserved_is_case_insensitive() {
        assert_eq!(
            strip_reserved_query_params("pg://h/db?Table=t"),
            "pg://h/db"
        );
    }
}
