//! `shpx convert <src> <dst>` の実装。

use shpx_core::{schema::find_geometry_column, Crs, Error, ReadOpts, Result, Uri, WriteOpts};
use shpx_geom::{parse_target_crs, Reprojector};

use crate::cli::ConvertArgs;
use crate::commands::parse_src_crs;
use crate::registry;

pub fn run(args: ConvertArgs) -> Result<()> {
    let src_uri = Uri::from_path(args.src.to_string_lossy().to_string());
    let dst_uri = Uri::from_path(args.dst.to_string_lossy().to_string());

    let src_driver =
        registry::select_driver(&src_uri).ok_or_else(|| registry::driver_not_found(&src_uri))?;
    let dst_driver =
        registry::select_driver(&dst_uri).ok_or_else(|| registry::driver_not_found(&dst_uri))?;

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

    let target_crs: Option<Crs> = args
        .reproject
        .as_deref()
        .map(parse_target_crs)
        .transpose()?;

    let mut reader = src_driver.open_read(&src_uri, &read_opts)?;
    let reader_schema = reader.schema();
    let src_crs: Option<Crs> = reader.crs().cloned();

    let reprojector = match (src_crs.as_ref(), target_crs.as_ref()) {
        (_, None) => None,
        (None, Some(_)) => {
            return Err(Error::Crs(
                "--reproject requires source CRS; specify --src-crs or use a source with embedded CRS"
                    .into(),
            ));
        }
        (Some(src), Some(tgt)) => {
            let r = Reprojector::new(src, tgt)?;
            if r.is_identity() {
                tracing::info!(target: "shpx::cli", "--reproject: src and target CRS match, skipping transform");
                None
            } else {
                Some(r)
            }
        }
    };

    // writer に渡す CRS とスキーマは、reproject 指定時は target に揃える。
    // 揃えないと writer が field metadata 経由で src CRS を拾い、出力ファイルの
    // CRS タグが入力のままになってしまう。
    let writer_crs: Option<Crs> = target_crs.clone().or_else(|| src_crs.clone());
    let writer_schema = if target_crs.is_some() {
        shpx_core::schema::replace_geometry_crs(&reader_schema, writer_crs.clone())?
    } else {
        reader_schema.clone()
    };

    let geom_idx = find_geometry_column(&reader_schema)?.map(|(i, _, _)| i);
    let mut writer = dst_driver.open_write(&dst_uri, writer_schema, writer_crs, &write_opts)?;

    let mut total_rows: u64 = 0;
    let mut total_batches: u64 = 0;
    for batch in reader.batches() {
        let batch = batch?;
        let batch = if let (Some(r), Some(gi)) = (reprojector.as_ref(), geom_idx) {
            r.transform_batch(&batch, gi)?
        } else {
            batch
        };
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
        reproject = reprojector.is_some(),
        "convert ok"
    );
    Ok(())
}
