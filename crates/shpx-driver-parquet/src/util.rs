//! ドライバ共通ユーティリティ。

use shpx_core::{Error, OnLoss, Result};

/// このドライバの識別名。`Error::Driver { name, .. }` 等で参照する。
pub const DRIVER_NAME: &str = "parquet";

/// 任意の `Display` を `Error::Driver` に詰める（`shpx_core::Error::driver` への薄いラッパ）。
pub fn driver_err<E: std::fmt::Display>(e: &E) -> Error {
    Error::driver(DRIVER_NAME, e)
}

/// 文字列メッセージから `Error::Driver` を作る。
pub fn driver_msg(msg: impl Into<String>) -> Error {
    Error::driver_msg(DRIVER_NAME, msg)
}

/// 損失種別の識別子。
///
/// v0.7 cycle 1 時点では空 (発火条件を満たす経路が現状の shpx に存在しないため):
/// - shpx 中間表現 (`shpx_geom::wkb::Geom`) は XY のみで Z/M は writer に到達しない
/// - Parquet は `coerce_types=false` のデフォルトで `Timestamp(Nanosecond)` と
///   `Decimal128(<= 38)` を完全保持する (`tests/roundtrip.rs` で裏付け)
///
/// `--parquet-coerce-types` 等のフラグや Z/M 中間表現を導入した時点で
/// `precision-on-parquet` / `nanosecond-truncation-on-parquet` / `z-on-parquet` /
/// `m-on-parquet` を順次追加する想定。
pub mod loss_kind {}

/// 損失検出時の挙動を 1 箇所で適用する。`shpx_rdb_common::apply_on_loss` への
/// 薄いラッパで、`tracing::warn!` の `target:` だけ Parquet 用に固定する。
///
/// cycle 1 時点では呼び出し箇所が存在しないため `dead_code`。将来 `loss_kind` に
/// 定数を追加した際に writer から呼ぶ。
#[allow(dead_code)]
pub(crate) fn apply_on_loss(kind: &'static str, field: &str, on_loss: OnLoss) -> Result<bool> {
    shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
        tracing::warn!(target: "shpx::parquet", kind, field, "lossy conversion");
    })
}
