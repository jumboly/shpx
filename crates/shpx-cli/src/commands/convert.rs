//! `shpx convert <src> <dst>` の実装。

use arrow_schema::SchemaRef;
use shpx_core::{
    schema::find_geometry_column, Crs, Driver, Error, LayerReader, LayerWriter, ReadOpts, Result,
    Uri, WriteOpts,
};
use shpx_geom::{parse_target_crs, Reprojector};

use crate::cli::{ConvertArgs, InsertModeArg};
use crate::commands::parse_src_crs;
use crate::registry;

fn build_read_opts(args: &ConvertArgs) -> Result<ReadOpts> {
    let select = if args.select.is_empty() {
        None
    } else {
        Some(args.select.clone())
    };
    Ok(ReadOpts {
        src_crs: parse_src_crs(args.src_crs.as_deref())?,
        encoding: args.encoding.clone(),
        where_clause: args.where_clause.clone(),
        select,
        query: args.query.clone(),
    })
}

fn build_write_opts(args: &ConvertArgs) -> WriteOpts {
    WriteOpts {
        encoding: args.encoding.clone(),
        on_loss: args.on_loss.into(),
        overwrite: args.overwrite,
        batch_size_hint: args.batch_size,
        create_table: args.create_table.into(),
        create_index: args.create_index.into(),
    }
}

/// PostGIS など RDB driver 専用の reader オプションが指定されたが、入力 driver が
/// それらを実際には消費しない可能性が高い場合（=非 URL 入力）に警告を出す。
/// 厳密判定は driver 側の責務だが、CLI 段階で「ファイル入力なのに `--where` 指定」を
/// 黙って捨てるとユーザー体験が悪いため、軽い警告だけ出しておく。
fn warn_filter_opts_on_file_uri(uri: &Uri, args: &ConvertArgs) {
    if uri.is_url() {
        return;
    }
    let warn = |opt: &str| {
        tracing::warn!(
            target: "shpx::cli",
            "{opt} is specified but `{}` does not look like an RDB URL; option will be ignored if the driver does not support it",
            uri.raw
        );
    };
    if args.where_clause.is_some() {
        warn("--where");
    }
    if !args.select.is_empty() {
        warn("--select");
    }
    if args.query.is_some() {
        warn("--query");
    }
}

struct BatchCounters {
    rows: u64,
    batches: u64,
}

/// 出力経路（bulk / batch）共通の入力。`Driver::open_*_write` への引数と、
/// reader 側の reproject / geometry index をまとめる。
struct PipelineCtx<'a> {
    dst_driver: &'a dyn Driver,
    dst_uri: &'a Uri,
    writer_schema: SchemaRef,
    writer_crs: Option<Crs>,
    write_opts: &'a WriteOpts,
    reader: &'a mut dyn LayerReader,
    reprojector: Option<&'a Reprojector>,
    geom_idx: Option<usize>,
}

fn run_bulk(ctx: PipelineCtx<'_>, counters: &mut BatchCounters) -> Result<()> {
    let PipelineCtx {
        dst_driver,
        dst_uri,
        writer_schema,
        writer_crs,
        write_opts,
        reader,
        reprojector,
        geom_idx,
    } = ctx;
    let mut bulk = dst_driver
        .open_bulk_write(dst_uri, writer_schema, writer_crs, write_opts)?
        .ok_or_else(|| {
            Error::driver_msg(
                dst_driver.name(),
                "driver advertised bulk_load but open_bulk_write returned None",
            )
        })?;
    let mut iter = reader.batches().map(|batch_res| {
        let batch = batch_res?;
        let batch = if let (Some(r), Some(gi)) = (reprojector, geom_idx) {
            r.transform_batch(&batch, gi)?
        } else {
            batch
        };
        counters.rows += batch.num_rows() as u64;
        counters.batches += 1;
        Result::Ok(batch)
    });
    bulk.bulk_write(&mut iter)?;
    bulk.finish()
}

fn run_batch(ctx: PipelineCtx<'_>, counters: &mut BatchCounters) -> Result<()> {
    let PipelineCtx {
        dst_driver,
        dst_uri,
        writer_schema,
        writer_crs,
        write_opts,
        reader,
        reprojector,
        geom_idx,
    } = ctx;
    let mut writer = dst_driver.open_write(dst_uri, writer_schema, writer_crs, write_opts)?;
    for batch in reader.batches() {
        let batch = batch?;
        let batch = if let (Some(r), Some(gi)) = (reprojector, geom_idx) {
            r.transform_batch(&batch, gi)?
        } else {
            batch
        };
        counters.rows += batch.num_rows() as u64;
        counters.batches += 1;
        writer.write_batch(&batch)?;
    }
    LayerWriter::finish(writer)
}

pub fn run(args: &ConvertArgs) -> Result<()> {
    let src_uri = Uri::from_path(args.src.clone());
    let dst_uri = Uri::from_path(args.dst.clone());

    let src_driver =
        registry::select_driver(&src_uri).ok_or_else(|| registry::driver_not_found(&src_uri))?;
    let dst_driver =
        registry::select_driver(&dst_uri).ok_or_else(|| registry::driver_not_found(&dst_uri))?;

    warn_filter_opts_on_file_uri(&src_uri, args);
    let read_opts = build_read_opts(args)?;
    let write_opts = build_write_opts(args);

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

    // 出力 driver が bulk 経路を持つかを capabilities で確認し、`--insert-mode` と組み合わせて分岐する。
    let bulk_supported = dst_driver.capabilities().bulk_load;
    let use_bulk = match args.insert_mode {
        InsertModeArg::Auto => bulk_supported,
        InsertModeArg::Bulk => {
            if !bulk_supported {
                return Err(Error::driver_msg(
                    dst_driver.name(),
                    "driver does not support bulk insert; use --insert-mode=batch or =auto",
                ));
            }
            true
        }
        InsertModeArg::Batch => false,
    };

    let mut counters = BatchCounters {
        rows: 0,
        batches: 0,
    };
    let ctx = PipelineCtx {
        dst_driver,
        dst_uri: &dst_uri,
        writer_schema,
        writer_crs,
        write_opts: &write_opts,
        reader: reader.as_mut(),
        reprojector: reprojector.as_ref(),
        geom_idx,
    };
    if use_bulk {
        run_bulk(ctx, &mut counters)?;
    } else {
        run_batch(ctx, &mut counters)?;
    }

    tracing::info!(
        target: "shpx::cli",
        src = %args.src,
        dst = %args.dst,
        from = src_driver.name(),
        to = dst_driver.name(),
        rows = counters.rows,
        batches = counters.batches,
        reproject = reprojector.is_some(),
        insert_mode = if use_bulk { "bulk" } else { "batch" },
        "convert ok"
    );
    Ok(())
}
