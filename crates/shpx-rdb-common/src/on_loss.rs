//! `OnLoss` ポリシーを 1 箇所で適用するヘルパー。
//!
//! `OnLoss::Error` → `Error::OnLoss` で中断、`OnLoss::Warn` → `warn_fn` を呼んで続行、
//! `OnLoss::Skip` → silent に skip シグナルを返す、という分岐は driver を跨いで同型。
//! 唯一 driver 固有なのは tracing target で、`tracing::warn!` の `target:` フィールドは
//! コンパイル時 const を要求するためマクロ呼び出しは driver 側に残し、warn 経路の挙動だけを
//! `warn_fn` クロージャで受け取る。

use shpx_core::{Error, OnLoss, Result};

/// 損失検出時の挙動を 1 箇所で適用する。
///
/// 戻り値:
/// - `Ok(true)`  — 続行（`Warn` 経路、`warn_fn` が呼ばれた直後）
/// - `Ok(false)` — その要素 (列・値) をスキップ（`warn_fn` は呼ばれない）
/// - `Err(_)`    — `OnLoss::Error` での中断（`warn_fn` は呼ばれない）
///
/// `warn_fn` は `OnLoss::Warn` のときだけ呼ばれる。各 driver は driver 固有の
/// tracing target をマクロに直書きしてここに渡す:
///
/// ```ignore
/// shpx_rdb_common::apply_on_loss(kind, field, on_loss, || {
///     tracing::warn!(target: "shpx::postgis", kind, field, "lossy conversion");
/// })
/// ```
pub fn apply_on_loss<F: FnOnce()>(
    kind: &'static str,
    field: &str,
    on_loss: OnLoss,
    warn_fn: F,
) -> Result<bool> {
    match on_loss {
        OnLoss::Error => Err(Error::OnLoss {
            kind: kind.to_string(),
            field: field.to_string(),
        }),
        OnLoss::Warn => {
            warn_fn();
            Ok(true)
        }
        OnLoss::Skip => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    const KIND: &str = "test-loss";

    #[test]
    fn error_returns_err_with_kind_and_field_and_no_warn_call() {
        let warned = Cell::new(false);
        let err = apply_on_loss(KIND, "col1", OnLoss::Error, || warned.set(true)).unwrap_err();
        match err {
            Error::OnLoss { kind, field } => {
                assert_eq!(kind, KIND);
                assert_eq!(field, "col1");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        assert!(!warned.get(), "warn_fn must not be called for Error");
    }

    #[test]
    fn warn_invokes_warn_fn_and_returns_true() {
        let warned = Cell::new(false);
        let r = apply_on_loss(KIND, "col1", OnLoss::Warn, || warned.set(true)).unwrap();
        assert!(r);
        assert!(warned.get(), "warn_fn must be called for Warn");
    }

    #[test]
    fn skip_returns_false_and_skips_warn_fn() {
        let warned = Cell::new(false);
        let r = apply_on_loss(KIND, "col1", OnLoss::Skip, || warned.set(true)).unwrap();
        assert!(!r);
        assert!(!warned.get(), "warn_fn must not be called for Skip");
    }
}
