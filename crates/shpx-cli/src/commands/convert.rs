//! `shpx convert <src> <dst>` の実装。

use shpx_core::{Error, ReadOpts, Result, Uri, WriteOpts};

use crate::cli::ConvertArgs;
use crate::commands::parse_src_crs;
use crate::registry;

pub fn run(args: ConvertArgs) -> Result<()> {
    let src_uri = Uri::from_path(args.src.to_string_lossy().to_string());
    let dst_uri = Uri::from_path(args.dst.to_string_lossy().to_string());

    let src_driver = registry::select_driver(&src_uri)
        .ok_or_else(|| no_driver_error(&src_uri.scheme, "input"))?;
    let dst_driver = registry::select_driver(&dst_uri)
        .ok_or_else(|| no_driver_error(&dst_uri.scheme, "output"))?;

    let read_opts = ReadOpts {
        src_crs: parse_src_crs(args.src_crs.as_deref())?,
        encoding: args.encoding.clone(),
    };
    let write_opts = WriteOpts {
        encoding: args.encoding,
        on_loss: args.on_loss.into(),
        overwrite: args.overwrite,
        batch_size_hint: args.batch_size,
    };

    let mut reader = src_driver.open_read(&src_uri, &read_opts)?;
    let schema = reader.schema();
    let crs = reader.crs().cloned();
    let mut writer = dst_driver.open_write(&dst_uri, schema, crs, &write_opts)?;

    let mut total_rows: u64 = 0;
    let mut total_batches: u64 = 0;
    for batch in reader.batches() {
        let batch = batch?;
        total_rows += batch.num_rows() as u64;
        total_batches += 1;
        writer.write_batch(&batch)?;
    }
    writer.finish()?;

    tracing::info!(
        target: "shpx::cli",
        src = %args.src.display(),
        dst = %args.dst.display(),
        from = src_driver.name(),
        to = dst_driver.name(),
        rows = total_rows,
        batches = total_batches,
        "convert ok"
    );
    Ok(())
}

fn no_driver_error(scheme: &str, side: &str) -> Error {
    if scheme.is_empty() {
        Error::Format(format!(
            "{side} has no extension; cannot infer driver"
        ))
    } else {
        Error::Format(format!(
            "no driver for {side} scheme `{scheme}`"
        ))
    }
}
