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

use crate::options::{AuthKind, ParsedUrl};
use crate::runtime::runtime;
use crate::util::{driver_err, driver_msg};

/// driver が保持する tiberius Client 型。compat 越しの tokio TcpStream を内包する。
pub type SqlClient = Client<Compat<TcpStream>>;

/// SQL Server の既定 TCP ポート。`?port=` のような URL クエリは受け付けない（URL の
/// `host:port` 形式から取る）。
pub const DEFAULT_PORT: u16 = 1433;

/// `mssql://...` URL を解析して `tiberius::Client` を返す。
///
/// 認証は `?auth=sql|integrated|windows` で切り替える (既定 `sql`、`?trusted_connection=true`
/// は `?auth=integrated` のエイリアス)。`integrated` / `windows` は CLI feature
/// `windows-auth` 有効時のみ動作 (Windows: pure Rust SSPI、Unix: system libgssapi-krb5
/// による Kerberos)。feature 無効時は明示エラーで案内する。
pub fn connect(url: &str) -> shpx_core::Result<SqlClient> {
    let parsed = ParsedUrl::parse(url)?;
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

    let auth = build_auth(parsed)?;
    cfg.authentication(auth);

    // dev / docker compose 環境では SQL Server の TLS 証明書が self-signed で発行される
    // ため、`trust_cert` を有効化する。プロダクション接続では URL クエリで CA を渡す
    // 拡張を v0.5+ で検討する。
    cfg.trust_cert();

    // tiberius default は LOGIN7 で 4096 バイトを request し、SQL Server がその値で
    // negotiate する。bulk insert では packet ごとに 1 round-trip 発生するため、4KB だと
    // BCP throughput が頭打ちになる (upstream PR #400 のベンチで 19.3M 行 bulk が 4KB →
    // 16KB で 186s → 108s、+42% に高速化を実証)。max 32767 を request しておけば SQL
    // Server 側で 16KB あたりに negotiate-down するので overshoot しても害はない。
    cfg.packet_size(32767);

    Ok(cfg)
}

/// `ParsedUrl` から `AuthMethod` を構築する。`AuthKind` ごとに必須フィールドを検証し、
/// feature 無効時は明示エラーで案内する。
fn build_auth(parsed: &ParsedUrl) -> shpx_core::Result<AuthMethod> {
    match parsed.auth {
        AuthKind::Sql => {
            let user = parsed.user.as_deref().ok_or_else(|| {
                driver_msg(
                    "mssql:// URL requires a username for SQL authentication \
                     (e.g. mssql://sa:password@host/db). For integrated auth use \
                     `?auth=integrated`.",
                )
            })?;
            let password = parsed.password.as_deref().unwrap_or("");
            Ok(AuthMethod::sql_server(user, password))
        }
        AuthKind::Integrated => build_integrated_auth(),
        AuthKind::Windows => build_windows_auth(parsed),
    }
}

// `Result` を返すのは feature 無効時の関数 (Err 必須) と signature を揃えるため。
// feature 有効時は常に `Ok` だが、cfg 違いで return 型が変わると呼び出し側で
// 分岐コードが必要になるので意図的に `Result` のままにする。
#[allow(clippy::unnecessary_wraps)]
#[cfg(any(
    all(windows, feature = "windows-auth"),
    all(unix, feature = "windows-auth"),
))]
fn build_integrated_auth() -> shpx_core::Result<AuthMethod> {
    Ok(AuthMethod::Integrated)
}

#[cfg(not(any(
    all(windows, feature = "windows-auth"),
    all(unix, feature = "windows-auth"),
)))]
fn build_integrated_auth() -> shpx_core::Result<AuthMethod> {
    Err(driver_msg(
        "?auth=integrated requires the `windows-auth` feature \
         (build with `cargo build --features windows-auth`). \
         On Unix this also requires system libgssapi-krb5.",
    ))
}

#[cfg(all(windows, feature = "windows-auth"))]
fn build_windows_auth(parsed: &ParsedUrl) -> shpx_core::Result<AuthMethod> {
    let user = parsed.user.as_deref().ok_or_else(|| {
        driver_msg(
            "?auth=windows requires a username (e.g. \
             mssql://DOMAIN%5Cuser:password@host/db?auth=windows). \
             `DOMAIN\\user` 形式の `\\` は URL では `%5C` に percent-encode する。",
        )
    })?;
    let password = parsed.password.as_deref().unwrap_or("");
    Ok(AuthMethod::windows(user, password))
}

#[cfg(not(all(windows, feature = "windows-auth")))]
fn build_windows_auth(_parsed: &ParsedUrl) -> shpx_core::Result<AuthMethod> {
    Err(driver_msg(
        "?auth=windows (NTLM) is only supported on Windows targets with the \
         `windows-auth` feature. On Unix, use `?auth=integrated` (Kerberos via libgssapi).",
    ))
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

    #[cfg(not(feature = "windows-auth"))]
    #[test]
    fn build_config_rejects_integrated_auth_without_feature() {
        // feature 無効時は build_config 段階で明示エラー (TCP 接続には進まない)。
        let parsed =
            ParsedUrl::parse("mssql://localhost/shpx_test?auth=integrated").unwrap();
        let err = build_config(&parsed).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("windows-auth"), "msg was: {msg}");
    }

    #[cfg(not(feature = "windows-auth"))]
    #[test]
    fn build_config_rejects_trusted_connection_alias_without_feature() {
        // 後方互換 alias も同じく build_config 段階で reject。
        let parsed =
            ParsedUrl::parse("mssql://localhost/shpx_test?trusted_connection=true").unwrap();
        let err = build_config(&parsed).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("windows-auth"), "msg was: {msg}");
    }

    #[cfg(all(unix, feature = "windows-auth"))]
    #[test]
    fn build_config_rejects_windows_auth_on_unix() {
        // Unix では `?auth=windows` (NTLM) は使えない。`?auth=integrated` を案内する。
        let parsed =
            ParsedUrl::parse("mssql://u:p@h/db?auth=windows").unwrap();
        let err = build_config(&parsed).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("only supported on Windows"), "msg was: {msg}");
    }

    #[cfg(all(unix, feature = "windows-auth"))]
    #[test]
    fn build_config_accepts_integrated_auth_on_unix_with_feature() {
        // `?auth=integrated` は feature 有効な Unix で `AuthMethod::Integrated` を生成。
        // 実際の Kerberos ticket 取得は libgssapi 任せで本テストでは検証しない。
        let parsed =
            ParsedUrl::parse("mssql://localhost/shpx_test?auth=integrated").unwrap();
        let cfg = build_config(&parsed).unwrap();
        assert_eq!(cfg.get_addr(), "localhost:1433");
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
