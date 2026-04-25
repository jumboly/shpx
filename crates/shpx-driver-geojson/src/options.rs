//! GeoJSON / GeoJSONL 固有オプションの解決。
//!
//! v0.2 サイクル 2 では指定可能なドライバ固有オプションが少ないため、
//! 暫定で **環境変数経由** で受け取る（CSV と同じ方針）。
//! 次サイクル以降 `WriteOpts.driver_specific` を導入する計画
//! （`docs/GEOJSON.md` の Future work 参照）。

use shpx_core::{ReadOpts, Result, Uri, WriteOpts};

use crate::util::driver_msg;

/// 環境変数: FeatureCollection 出力時に pretty-print する (`true` / `false`)。既定 `false`。
/// GeoJSONL では 1 feature/line 制約があるため指定しても無視される。
pub const ENV_PRETTY: &str = "SHPX_GEOJSON_PRETTY";

/// 出力形式。`Uri::scheme` から決定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// `.geojson` — 単一の `FeatureCollection` オブジェクト。全 Feature をカンマ区切りの配列に格納。
    FeatureCollection,
    /// `.geojsonl` / `.ndjson` / `.jsonl` — 1 行 = 1 Feature の改行区切り JSON。
    Lines,
}

impl OutputFormat {
    /// scheme（拡張子の小文字化結果）から出力形式を決める。
    /// 未知 scheme は `FeatureCollection` を既定にする（呼び出し元で scheme は検証済みの想定）。
    #[must_use]
    pub fn from_scheme(scheme: &str) -> Self {
        match scheme {
            "geojsonl" | "ndjson" | "jsonl" => Self::Lines,
            // "geojson" およびその他は FeatureCollection 扱い。
            _ => Self::FeatureCollection,
        }
    }
}

/// 解決済みの読み出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedReadOpts {
    pub format: OutputFormat,
    pub src_crs: Option<shpx_core::Crs>,
}

impl ResolvedReadOpts {
    pub fn resolve(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        Ok(Self {
            format: OutputFormat::from_scheme(&uri.scheme),
            src_crs: opts.src_crs.clone(),
        })
    }
}

/// 解決済みの書き出しオプション。
#[derive(Debug, Clone)]
pub struct ResolvedWriteOpts {
    pub format: OutputFormat,
    pub on_loss: shpx_core::OnLoss,
    pub overwrite: bool,
    /// FeatureCollection 出力時のみ意味を持つ。GeoJSONL では無視。
    pub pretty: bool,
}

impl ResolvedWriteOpts {
    pub fn resolve(uri: &Uri, opts: &WriteOpts) -> Result<Self> {
        Ok(Self {
            format: OutputFormat::from_scheme(&uri.scheme),
            on_loss: opts.on_loss,
            overwrite: opts.overwrite,
            pretty: resolve_pretty()?,
        })
    }
}

fn resolve_pretty() -> Result<bool> {
    match std::env::var(ENV_PRETTY) {
        Ok(raw) => match raw.to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Ok(true),
            "false" | "0" | "no" | "" => Ok(false),
            other => Err(driver_msg(format!(
                "{ENV_PRETTY}: invalid value `{other}` (expected true/false)"
            ))),
        },
        Err(_) => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheme_geojson_is_feature_collection() {
        assert_eq!(
            OutputFormat::from_scheme("geojson"),
            OutputFormat::FeatureCollection
        );
    }

    #[test]
    fn lines_schemes_map_to_lines() {
        for s in ["geojsonl", "ndjson", "jsonl"] {
            assert_eq!(OutputFormat::from_scheme(s), OutputFormat::Lines, "{s}");
        }
    }

    #[test]
    fn pretty_default_is_false() {
        // 環境変数の影響を排除するため値を保存して復元する。
        let saved = std::env::var(ENV_PRETTY).ok();
        std::env::remove_var(ENV_PRETTY);
        assert!(!resolve_pretty().unwrap());
        if let Some(v) = saved {
            std::env::set_var(ENV_PRETTY, v);
        }
    }
}
