//! `shpx info <src>` の実装。

use shpx_core::{schema::find_geometry_column, Crs, ReadOpts, Result, Uri};

use crate::cli::InfoArgs;
use crate::commands::parse_src_crs;
use crate::registry;

pub fn run(args: InfoArgs) -> Result<()> {
    let uri = Uri::from_path(args.src.to_string_lossy().to_string());
    let driver = registry::select_driver(&uri).ok_or_else(|| registry::driver_not_found(&uri))?;

    let opts = ReadOpts {
        src_crs: parse_src_crs(args.src_crs.as_deref())?,
        encoding: args.encoding,
    };
    let reader = driver.open_read(&uri, &opts)?;
    let schema = reader.schema();
    let crs = reader.crs();

    println!("driver:  {}", driver.name());
    match reader.row_count_hint() {
        Some(n) => println!("rows:    {n}"),
        None => println!("rows:    (unknown)"),
    }
    println!("crs:     {}", format_crs(crs));

    match find_geometry_column(&schema)? {
        Some((_, name, meta)) => println!(
            "geometry: {name} ({:?}, encoding={:?})",
            meta.geometry_type, meta.encoding
        ),
        None => println!("geometry: (none)"),
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
    let Some(c) = crs else {
        return "(none)".to_string();
    };
    if let Some(code) = c.epsg_code() {
        return format!("EPSG:{code}");
    }
    match c.wkt.as_deref() {
        Some(w) => {
            let head: String = w.chars().take(60).collect();
            format!("WKT: {head}…")
        }
        None => "(unknown)".to_string(),
    }
}
