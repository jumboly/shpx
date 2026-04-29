//! SQL Server 接続のラッパ。
//!
//! `mssql://user:pass@host:1433/db?key=value` を自前で分解し、`tiberius::Config`
//! を構築する。tokio-postgres と違って tiberius は URL 文字列を直接受け付けず、
//! `Config::host/port/database/authentication` で個別に設定する API なので、
//! driver 側でパースする必要がある。
//!
//! tiberius は futures-io ベースの `AsyncRead` / `AsyncWrite` を要求するため、
//! tokio TcpStream を `tokio_util::compat::TokioAsyncWriteCompatExt` 経由で
//! ブリッジしてから `Client::connect` に渡す。

use tiberius::{AuthMethod, Client, Config};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

use crate::options::ParsedUrl;
use crate::runtime::runtime;
use crate::util::{driver_err, driver_msg};

/// driver が保持する tiberius Client 型。compat 越しの tokio TcpStream を内包する。
pub type SqlClient = Client<Compat<TcpStream>>;

/// SQL Server の既定 TCP ポート。`?port=` のような URL クエリは受け付けない（URL の
/// `host:port` 形式から取る）。
pub const DEFAULT_PORT: u16 = 1433;

/// `mssql://...` URL を解析して `tiberius::Client` を返す。
///
/// 認証は cycle 1 では SQL 認証 (`user:pass`) のみサポート。`?trusted_connection=true`
/// は driver 段階で受け付けるが、未対応エラーを返す（v0.5+ で実装予定）。
pub fn connect(url: &str) -> shpx_core::Result<SqlClient> {
    let parsed = ParsedUrl::parse(url)?;

    if parsed.trusted_connection {
        return Err(driver_msg(
            "?trusted_connection=true (Windows authentication) is not supported \
             in v0.4. Use SQL authentication (`mssql://user:pass@host/db?...`)",
        ));
    }

    let cfg = build_config(&parsed)?;
    let rt = runtime()?;

    let client = rt.block_on(async {
        let tcp = TcpStream::connect(cfg.get_addr())
            .await
            .map_err(|e| driver_err(&e))?;
        // SQL Server は connect 直後に TLS 1.2 ハンドシェイクの prelogin を行うため、
        // Nagle アルゴリズム由来のレイテンシを避ける（tiberius 公式サンプルの推奨）。
        tcp.set_nodelay(true).map_err(|e| driver_err(&e))?;
        Client::connect(cfg, tcp.compat_write())
            .await
            .map_err(|e| driver_err(&e))
    })?;

    Ok(client)
}

/// 解析済み URL から `tiberius::Config` を構築する。テスト可能性のため `connect` から分離。
pub(crate) fn build_config(parsed: &ParsedUrl) -> shpx_core::Result<Config> {
    let mut cfg = Config::new();
    cfg.host(&parsed.host);
    cfg.port(parsed.port.unwrap_or(DEFAULT_PORT));
    if let Some(db) = &parsed.database {
        cfg.database(db);
    }

    let user = parsed.user.as_deref().ok_or_else(|| {
        driver_msg(
            "mssql:// URL requires a username for SQL authentication \
             (e.g. mssql://sa:password@host/db)",
        )
    })?;
    let password = parsed.password.as_deref().unwrap_or("");
    cfg.authentication(AuthMethod::sql_server(user, password));

    // dev / docker compose 環境では SQL Server の TLS 証明書が self-signed で発行される
    // ため、`trust_cert` を有効化する。プロダクション接続では URL クエリで CA を渡す
    // 拡張を v0.5+ で検討する。
    cfg.trust_cert();

    Ok(cfg)
}

/// `tiberius::Client::simple_query` の同期ラッパ。schema probe など bind 不要な静的 SQL に使う。
pub fn simple_query(client: &mut SqlClient, sql: impl Into<String>) -> shpx_core::Result<()> {
    let rt = runtime()?;
    let sql = sql.into();
    rt.block_on(async {
        client
            .simple_query(sql)
            .await
            .map_err(|e| driver_err(&e))?
            .into_results()
            .await
            .map_err(|e| driver_err(&e))?;
        Ok::<_, shpx_core::Error>(())
    })?;
    let _ = client; // borrow を保持
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::ParsedUrl;

    #[test]
    fn build_config_sets_host_port_database() {
        let parsed = ParsedUrl::parse("mssql://sa:pw@localhost:1433/shpx_test").unwrap();
        let cfg = build_config(&parsed).unwrap();
        // tiberius `Config` は内部状態を直接観察できないため、`get_addr` の文字列で確認。
        assert_eq!(cfg.get_addr(), "localhost:1433");
    }

    #[test]
    fn build_config_uses_default_port_when_omitted() {
        let parsed = ParsedUrl::parse("mssql://sa:pw@localhost/shpx_test").unwrap();
        let cfg = build_config(&parsed).unwrap();
        assert_eq!(cfg.get_addr(), "localhost:1433");
    }

    #[test]
    fn build_config_errors_when_user_missing() {
        let parsed = ParsedUrl::parse("mssql://localhost/shpx_test").unwrap();
        let err = build_config(&parsed).unwrap_err();
        assert!(format!("{err}").contains("username"));
    }

    #[test]
    fn connect_rejects_trusted_connection_in_v04() {
        // `?trusted_connection=true` は v0.4 では未対応。connect が早期に reject する。
        let err = connect("mssql://sa:pw@localhost/shpx_test?trusted_connection=true").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("trusted_connection"), "msg was: {msg}");
    }

    /// `tiberius::Config::new()` の生成が壊れていないことだけ確認するスモークテスト
    /// （実際の接続はしない）。
    #[test]
    fn tiberius_config_smoke() {
        let mut cfg = Config::new();
        cfg.host("localhost");
        cfg.port(1433);
        assert_eq!(cfg.get_addr(), "localhost:1433");
    }
}
