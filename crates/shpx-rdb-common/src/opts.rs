//! WriteOpts の整合性チェック（CREATE TABLE 戦略との衝突など）。

use shpx_core::{CreateTable, Error, Result, WriteOpts};

/// `--overwrite` と `create_table=Never` の組み合わせを reject する。
///
/// `--overwrite` は DROP→CREATE の挙動を要求するが、`Never` は CREATE 発行を禁止する。
/// 同時指定すると DROP した直後に空の名前空間へ INSERT を試みることになり破綻するので、
/// CLI を抜けた driver の入口で早めに弾く。
///
/// `driver_name` はエラーの `Error::Driver` に渡される識別名（`"postgis"` / `"sqlserver"`）。
pub fn validate_overwrite_compat(opts: &WriteOpts, driver_name: &'static str) -> Result<()> {
    if opts.overwrite && matches!(opts.create_table, CreateTable::Never) {
        return Err(Error::driver_msg(
            driver_name,
            "--overwrite と --create-table=never は同時に指定できない".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_overwrite_compat_passes_for_default() {
        validate_overwrite_compat(&WriteOpts::default(), "test").unwrap();
    }

    #[test]
    fn validate_overwrite_compat_passes_when_overwrite_with_always() {
        let opts = WriteOpts {
            overwrite: true,
            create_table: CreateTable::Always,
            ..Default::default()
        };
        validate_overwrite_compat(&opts, "test").unwrap();
    }

    #[test]
    fn validate_overwrite_compat_rejects_overwrite_with_never() {
        let opts = WriteOpts {
            overwrite: true,
            create_table: CreateTable::Never,
            ..Default::default()
        };
        let err = validate_overwrite_compat(&opts, "test").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("--overwrite"), "msg was: {msg}");
        assert!(msg.contains("--create-table=never"), "msg was: {msg}");
    }

    #[test]
    fn validate_overwrite_compat_passes_when_only_never() {
        let opts = WriteOpts {
            overwrite: false,
            create_table: CreateTable::Never,
            ..Default::default()
        };
        validate_overwrite_compat(&opts, "test").unwrap();
    }
}
