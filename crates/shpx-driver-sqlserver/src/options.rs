//! SQL Server 固有オプション。
//!
//! - 接続 URL: `mssql://user:pass@host:1433/db?table=...&geom_type=...`
//! - テーブル名は URI クエリ `?table=schema.name` または環境変数 `SHPX_MSSQL_TABLE`
//!   で指定する。schema 省略時は `dbo`（SQL Server の既定スキーマ）。
//! - geometry / geography 切替は `?geom_type=geometry|geography`（既定 `geometry`、
//!   未指定で schema metadata に `edges=spherical` がある場合のみ writer 側で
//!   `geography` に上書きする）。
//! - `?trusted_connection=true` は CLI からの形だけ受理し、`conn::connect` で
//!   未対応エラーにする（v0.5+ で Windows 認証を実装予定）。

use shpx_core::{CreateIndex, CreateTable, ReadOpts, Result, Uri, WriteOpts};

use crate::util::{driver_msg, DRIVER_NAME};

/// 環境変数: 入出力対象のテーブル名（`<schema>.<name>` 可）。URI クエリより優先度低。
pub const ENV_TABLE: &str = "SHPX_MSSQL_TABLE";

/// 環境変数: bulk loader の chunk size（行数）。cycle 2 で利用。未設定時は既定 100,000。
pub const ENV_BULK_CHUNK: &str = "SHPX_MSSQL_BULK_CHUNK";

/// staging bulk の既定 chunk size。Express edition / 低メモリ dev 環境で安全側。
/// bench 時のみ env で 1,000,000 などに引き上げる運用。
pub const DEFAULT_BULK_CHUNK: usize = 100_000;

/// SQL Server の geometry 列に使う UDT 種別。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum GeomKind {
    /// `geometry` (平面座標、SRID 0 でも有効)。既定。
    #[default]
    Geometry,
    /// `geography` (地理座標、有効な geographic CRS が必須、既定 SRID 4326)。
    Geography,
}

impl GeomKind {
    /// T-SQL 上の UDT 名 (`geometry` / `geography`)。CAST / 関数呼び出しで使う。
    #[must_use]
    pub fn t_sql_name(self) -> &'static str {
        match self {
            Self::Geometry => "geometry",
            Self::Geography => "geography",
        }
    }
}

/// `mssql://...` を分解した中間表現。`conn::build_config` と `options::resolve` の両方で利用。
#[derive(Debug, Clone, Default)]
pub struct ParsedUrl {
    pub user: Option<String>,
    pub password: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    pub database: Option<String>,
    /// `?table=schema.name` または `?table=name`（未指定なら None）。
    pub table: Option<String>,
    /// `?geom_type=geometry|geography`（未指定なら None、writer/reader 側で既定を補う）。
    pub geom_type: Option<GeomKind>,
    /// `?trusted_connection=true`（v0.5+ 予約、cycle 1 は受け取って即 reject）。
    pub trusted_connection: bool,
}

impl ParsedUrl {
    pub fn parse(url: &str) -> Result<Self> {
        let rest = url.strip_prefix("mssql://").ok_or_else(|| {
            driver_msg(format!(
                "{DRIVER_NAME}: URL must start with `mssql://` (got `{url}`)"
            ))
        })?;
        let (authority_path, query) = rest.split_once('?').map_or((rest, ""), |(a, q)| (a, q));

        // userinfo@host[:port]/db を分解する。authority と path の境界は最初の `/`。
        let (authority, path) = authority_path
            .split_once('/')
            .map_or((authority_path, ""), |(a, p)| (a, p));

        // userinfo は最後の `@` を区切りとする (password に `@` を含む場合は percent encode が必要)。
        let (userinfo, hostport) = match authority.rfind('@') {
            Some(idx) => (Some(&authority[..idx]), &authority[idx + 1..]),
            None => (None, authority),
        };

        let (host, port) = parse_host_port(hostport)?;
        let (user, password) = match userinfo {
            Some(s) => parse_userinfo(s),
            None => (None, None),
        };

        let database = if path.is_empty() {
            None
        } else {
            Some(percent_decode(path))
        };

        let mut parsed = Self {
            user,
            password,
            host,
            port,
            database,
            ..Self::default()
        };
        parsed.apply_query(query)?;
        Ok(parsed)
    }

    fn apply_query(&mut self, query: &str) -> Result<()> {
        if query.is_empty() {
            return Ok(());
        }
        for pair in query.split('&') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            let key_lc = k.to_ascii_lowercase();
            match key_lc.as_str() {
                "table" => {
                    if v.is_empty() {
                        return Err(driver_msg(format!(
                            "{DRIVER_NAME}: empty `?table=` in URI query"
                        )));
                    }
                    self.table = Some(percent_decode(v));
                }
                "geom_type" => {
                    let decoded = percent_decode(v).to_ascii_lowercase();
                    self.geom_type = Some(parse_geom_type(&decoded)?);
                }
                "trusted_connection" => {
                    let decoded = percent_decode(v).to_ascii_lowercase();
                    self.trusted_connection = matches!(decoded.as_str(), "true" | "1" | "yes");
                }
                _ => {
                    // 未知のクエリパラメタは無視する。tiberius 拡張用に将来枠を残す。
                }
            }
        }
        Ok(())
    }
}

fn parse_geom_type(s: &str) -> Result<GeomKind> {
    match s {
        "geometry" => Ok(GeomKind::Geometry),
        "geography" => Ok(GeomKind::Geography),
        other => Err(driver_msg(format!(
            "{DRIVER_NAME}: unknown ?geom_type=`{other}` (must be `geometry` or `geography`)"
        ))),
    }
}

fn parse_host_port(s: &str) -> Result<(String, Option<u16>)> {
    if s.is_empty() {
        return Err(driver_msg(format!("{DRIVER_NAME}: missing host in URL")));
    }
    if let Some((h, p)) = s.split_once(':') {
        let port: u16 = p.parse().map_err(|_| {
            driver_msg(format!(
                "{DRIVER_NAME}: invalid port `{p}` in URL (must be 0-65535)"
            ))
        })?;
        Ok((percent_decode(h), Some(port)))
    } else {
        Ok((percent_decode(s), None))
    }
}

fn parse_userinfo(s: &str) -> (Option<String>, Option<String>) {
    if let Some((u, p)) = s.split_once(':') {
        (Some(percent_decode(u)), Some(percent_decode(p)))
    } else if s.is_empty() {
        (None, None)
    } else {
        (Some(percent_decode(s)), None)
    }
}

/// 解決済みの読み出しオプション（v0.4 cycle 1 は table モード固定）。
#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
    /// 接続 URL（`mssql://...`）。
    pub url: String,
    /// テーブルが属する schema 名（既定 `dbo`）。
    pub schema: String,
    /// テーブル名。
    pub table: String,
    /// CLI `--src-crs` 由来の CRS 補完。
    pub src_crs: Option<shpx_core::Crs>,
    /// `?geom_type=` で明示された UDT 種別。reader は実 UDT 名を `INFORMATION_SCHEMA`
    /// から検出するため、これは「URL の主張」として保持するだけ。
    pub geom_type_hint: Option<GeomKind>,
}

impl ResolvedReadOpts {
    pub fn resolve(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let parsed = ParsedUrl::parse(uri.path())?;
        let qualified = resolve_table(parsed.table.as_deref())?;
        let (schema, table) = split_qualified(&qualified);
        Ok(Self {
            url: uri.path().to_string(),
            schema,
            table,
            src_crs: opts.src_crs.clone(),
            geom_type_hint: parsed.geom_type,
        })
    }
}

/// 解決済みの書き出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub url: String,
    pub schema: String,
    pub table: String,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
    pub create_table: CreateTable,
    pub create_index: CreateIndex,
    /// 既定 `Geometry`。`?geom_type=geography` 指定または schema metadata の
    /// `edges=spherical` で `Geography` に切り替わる（後者は writer 側で適用）。
    pub geom_type: GeomKind,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
        // PostGIS と同じく `--overwrite` は DROP→CREATE を要求するため
        // `Never` (CREATE 発行禁止) と矛盾する。CLI 段階で気付かせるため早期に reject。
        if opts.overwrite && matches!(opts.create_table, CreateTable::Never) {
            return Err(driver_msg(format!(
                "{DRIVER_NAME}: --overwrite と --create-table=never は同時に指定できない"
            )));
        }
        let parsed = ParsedUrl::parse(uri.path())?;
        let qualified = resolve_table(parsed.table.as_deref())?;
        let (schema, table) = split_qualified(&qualified);
        Ok(Self {
            url: uri.path().to_string(),
            schema,
            table,
            on_loss: opts.on_loss,
            overwrite: opts.overwrite,
            create_table: opts.create_table,
            create_index: opts.create_index,
            geom_type: parsed.geom_type.unwrap_or_default(),
        })
    }
}

/// テーブル名（`schema.name` または `name`）を解決する。
///
/// 優先順位: URL クエリ `?table=...` > 環境変数 `SHPX_MSSQL_TABLE`。
/// どちらも無い場合はエラー（v0.4 reader/writer は table 名必須）。
fn resolve_table(query_table: Option<&str>) -> Result<String> {
    if let Some(t) = query_table {
        return Ok(t.to_string());
    }
    match std::env::var(ENV_TABLE) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => Err(driver_msg(format!(
            "{DRIVER_NAME}: ?table=... query parameter is required (or set {ENV_TABLE})"
        ))),
    }
}

/// `schema.name` を `(schema, name)` に分割。schema 省略時は `dbo`（SQL Server の既定）。
fn split_qualified(s: &str) -> (String, String) {
    if let Some((schema, name)) = s.split_once('.') {
        (schema.to_string(), name.to_string())
    } else {
        ("dbo".to_string(), s.to_string())
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push((h << 4) | l);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_url() {
        let p = ParsedUrl::parse("mssql://sa:Pw1@localhost:1433/shpx_test?table=dbo.t").unwrap();
        assert_eq!(p.user.as_deref(), Some("sa"));
        assert_eq!(p.password.as_deref(), Some("Pw1"));
        assert_eq!(p.host, "localhost");
        assert_eq!(p.port, Some(1433));
        assert_eq!(p.database.as_deref(), Some("shpx_test"));
        assert_eq!(p.table.as_deref(), Some("dbo.t"));
    }

    #[test]
    fn parse_omits_port_when_absent() {
        let p = ParsedUrl::parse("mssql://sa:Pw1@localhost/db?table=t").unwrap();
        assert_eq!(p.port, None);
    }

    #[test]
    fn parse_geom_type_geography() {
        let p = ParsedUrl::parse("mssql://sa:Pw@h/db?table=t&geom_type=geography").unwrap();
        assert_eq!(p.geom_type, Some(GeomKind::Geography));
    }

    #[test]
    fn parse_geom_type_default_geometry_when_explicit() {
        let p = ParsedUrl::parse("mssql://sa:Pw@h/db?table=t&geom_type=geometry").unwrap();
        assert_eq!(p.geom_type, Some(GeomKind::Geometry));
    }

    #[test]
    fn parse_geom_type_unknown_value_errors() {
        let err = ParsedUrl::parse("mssql://sa:Pw@h/db?table=t&geom_type=points").unwrap_err();
        assert!(format!("{err}").contains("unknown ?geom_type"));
    }

    #[test]
    fn parse_trusted_connection_is_received() {
        // 値は受け取るだけ。connect 側で reject する。
        let p = ParsedUrl::parse("mssql://sa:Pw@h/db?trusted_connection=true&table=t").unwrap();
        assert!(p.trusted_connection);
    }

    #[test]
    fn parse_password_with_at_sign_uses_last_at() {
        // password に `@` を含む場合は URL spec に従い最後の `@` を区切りに使う。
        // 実用上は percent encode (%40) を勧めるが、保守的に最後の `@` で分割。
        let p = ParsedUrl::parse("mssql://sa:p@ss@host/db?table=t").unwrap();
        assert_eq!(p.user.as_deref(), Some("sa"));
        assert_eq!(p.password.as_deref(), Some("p@ss"));
        assert_eq!(p.host, "host");
    }

    #[test]
    fn parse_percent_decoded_password() {
        let p = ParsedUrl::parse("mssql://sa:Sh%21px_pw@h/db?table=t").unwrap();
        assert_eq!(p.password.as_deref(), Some("Sh!px_pw"));
    }

    #[test]
    fn parse_rejects_non_mssql_scheme() {
        let err = ParsedUrl::parse("pg://h/db?table=t").unwrap_err();
        assert!(format!("{err}").contains("must start with `mssql://`"));
    }

    #[test]
    fn parse_rejects_invalid_port() {
        let err = ParsedUrl::parse("mssql://h:abc/db").unwrap_err();
        assert!(format!("{err}").contains("invalid port"));
    }

    #[test]
    fn parse_no_userinfo() {
        // userinfo 無し → user/password とも None（connect で error になる）。
        let p = ParsedUrl::parse("mssql://localhost/db?table=t").unwrap();
        assert!(p.user.is_none() && p.password.is_none());
        assert_eq!(p.host, "localhost");
    }

    #[test]
    fn split_qualified_defaults_to_dbo() {
        assert_eq!(
            split_qualified("cities"),
            ("dbo".to_string(), "cities".to_string())
        );
        assert_eq!(
            split_qualified("foo.bar"),
            ("foo".to_string(), "bar".to_string())
        );
    }

    #[test]
    fn resolve_read_default() {
        let uri = Uri::from_path("mssql://sa:pw@h/db?table=t");
        let resolved = ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).unwrap();
        assert_eq!(resolved.schema, "dbo");
        assert_eq!(resolved.table, "t");
        assert_eq!(resolved.geom_type_hint, None);
    }

    #[test]
    fn resolve_write_default_keeps_if_not_exists() {
        let uri = Uri::from_path("mssql://sa:pw@h/db?table=t");
        let resolved = ResolvedWriteOpts::resolve(&uri, &WriteOpts::default()).unwrap();
        assert_eq!(resolved.create_table, CreateTable::IfNotExists);
        assert_eq!(resolved.create_index, CreateIndex::Auto);
        assert_eq!(resolved.geom_type, GeomKind::Geometry);
    }

    #[test]
    fn resolve_write_geography_via_url() {
        let uri = Uri::from_path("mssql://sa:pw@h/db?table=t&geom_type=geography");
        let resolved = ResolvedWriteOpts::resolve(&uri, &WriteOpts::default()).unwrap();
        assert_eq!(resolved.geom_type, GeomKind::Geography);
    }

    #[test]
    fn resolve_write_rejects_overwrite_with_never() {
        let uri = Uri::from_path("mssql://sa:pw@h/db?table=t");
        let opts = WriteOpts {
            overwrite: true,
            create_table: CreateTable::Never,
            ..Default::default()
        };
        let err = ResolvedWriteOpts::resolve(&uri, &opts).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--overwrite"), "msg was: {msg}");
        assert!(msg.contains("--create-table=never"), "msg was: {msg}");
    }

    #[test]
    fn resolve_table_missing_errors() {
        // 環境変数が未設定で query にも無ければエラー。env が設定済みの環境では skip。
        if std::env::var(ENV_TABLE).is_ok() {
            return;
        }
        let uri = Uri::from_path("mssql://sa:pw@h/db");
        assert!(ResolvedReadOpts::resolve(&uri, &ReadOpts::default()).is_err());
    }

    #[test]
    fn t_sql_name_for_geom_kind() {
        assert_eq!(GeomKind::Geometry.t_sql_name(), "geometry");
        assert_eq!(GeomKind::Geography.t_sql_name(), "geography");
    }
}
