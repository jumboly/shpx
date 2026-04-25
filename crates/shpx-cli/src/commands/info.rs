//! `shpx info <src>` の実装。

use shpx_core::{
    schema::{GeometryMeta, GEOMETRY_META_KEY},
    Crs, ReadOpts, Result, Uri,
};

use crate::cli::InfoArgs;
use crate::commands::parse_src_crs;
use crate::registry;

pub fn run(args: InfoArgs) -> Result<()> {
    let uri = Uri::from_path(args.src.to_string_lossy().to_string());
    let driver = registry::select_driver(&uri).ok_or_else(|| {
        shpx_core::Error::Format(if uri.scheme.is_empty() {
            "input has no extension; cannot infer driver".to_string()
        } else {
            format!("no driver for input scheme `{}`", uri.scheme)
        })
    })?;

    let opts = ReadOpts {
        src_crs: parse_src_crs(args.src_crs.as_deref())?,
        encoding: args.encoding,
    };
    let reader = driver.open_read(&uri, &opts)?;
    let schema = reader.schema();
    let crs = reader.crs();
    let row_count = reader.row_count_hint();

    println!("driver:  {}", driver.name());
    match row_count {
        Some(n) => println!("rows:    {n}"),
        None => println!("rows:    (unknown)"),
    }
    println!("crs:     {}", format_crs(crs));

    let geom = find_geometry(&schema)?;
    if let Some((name, meta)) = geom {
        println!(
            "geometry: {name} ({:?}, encoding={:?})",
            meta.geometry_type, meta.encoding
        );
    } else {
        println!("geometry: (none)");
    }

    println!("schema:");
    let n_width = schema.fields().len().to_string().len();
    let name_width = schema
        .fields()
        .iter()
        .map(|f| f.name().len())
        .max()
        .unwrap_or(0);
    for (i, f) in schema.fields().iter().enumerate() {
        let nullable = if f.is_nullable() { "nullable" } else { "not null" };
        println!(
            "  {idx:>n_width$}: {name:<name_width$}  {ty:?}  {nullable}",
            idx = i + 1,
            name = f.name(),
            ty = f.data_type(),
        );
    }
    Ok(())
}

fn format_crs(crs: Option<&Crs>) -> String {
    match crs {
        None => "(none)".to_string(),
        Some(c) => match c.epsg_code() {
            Some(code) => format!("EPSG:{code}"),
            None => match c.wkt.as_deref() {
                Some(w) => {
                    let head: String = w.chars().take(60).collect();
                    format!("WKT: {head}…")
                }
                None => "(unknown)".to_string(),
            },
        },
    }
}

fn find_geometry(
    schema: &arrow_schema::SchemaRef,
) -> Result<Option<(String, GeometryMeta)>> {
    for f in schema.fields() {
        if let Some(json) = f.metadata().get(GEOMETRY_META_KEY) {
            return Ok(Some((f.name().clone(), GeometryMeta::from_json(json)?)));
        }
    }
    Ok(None)
}
