//! shpx 全体で共有するエラー型。
//!
//! `thiserror` を使い、CLI から最終ユーザーへ出すメッセージと、
//! 各 driver が伝搬する詳細メッセージを 1 つの enum に集約する。

use thiserror::Error;

/// shpx の fallible API が返す統一エラー型。
#[derive(Debug, Error)]
pub enum Error {
    /// 下層 I/O エラー（ファイル / ソケット / etc）。
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Arrow スキーマや field metadata の不整合。
    #[error("schema error: {0}")]
    Schema(String),

    /// 出力先フォーマットでサポート外の型を書こうとした（`--on-loss` 適用前の純粋判定）。
    #[error("unsupported type: cannot map `{from}` to `{to}` (field `{field}`)")]
    UnsupportedType {
        /// 入力側の Arrow 型を表す文字列（例: `Timestamp(Microsecond, None)`）。
        from: String,
        /// 出力先フォーマット名（例: `dbf`）。
        to: String,
        /// 該当フィールド名。
        field: String,
    },

    /// WKB encode/decode、ジオメトリ型不整合、空ジオメトリ拒否など。
    #[error("geometry error: {0}")]
    Geometry(String),

    /// CRS 解釈不能（WKT1 から EPSG が拾えず `--src-crs` も無い等）。
    #[error("CRS error: {0}")]
    Crs(String),

    /// `--on-loss=error` で打ち切られた損失変換。
    ///
    /// `kind` は損失の種類（例: `"binary-on-shp"`、`"timestamp-on-shp"`）。
    #[error("loss-prevented abort: {kind} on field `{field}`")]
    OnLoss {
        /// 損失種別の識別子。
        kind: String,
        /// 該当フィールド名。
        field: String,
    },

    /// 各 Driver から伝搬したフォーマット固有エラー。
    #[error("driver `{name}`: {msg}")]
    Driver {
        /// Driver 識別名（`Driver::name()` の戻り値）。
        name: &'static str,
        /// フォーマット固有のメッセージ。
        msg: String,
    },

    /// CLI 入力や URI 解釈などのフォーマット系エラー。
    #[error("format error: {0}")]
    Format(String),
}

/// shpx 標準の Result 型エイリアス。
pub type Result<T> = std::result::Result<T, Error>;
