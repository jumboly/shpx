//! `shpx drivers` の実装。登録 driver と capabilities を `--format=text|json` で
//! 一覧表示する。`json` は CI / scripting 向けの `[{ name, schemes, capabilities }]`
//! 配列で、`Capabilities` の `serde::Serialize` 出力をそのまま `capabilities` に
//! 入れる。

use serde::Serialize;
use shpx_core::{Capabilities, Result, StringEncoding};

use crate::cli::{DriversArgs, OutputFormatArg};
use crate::commands::serialize_json;
use crate::registry;

pub fn run(args: &DriversArgs) -> Result<()> {
    match args.format {
        OutputFormatArg::Text => {
            print_text();
            Ok(())
        }
        OutputFormatArg::Json => print_json(args.pretty),
    }
}

fn print_text() {
    let drivers = registry::all_drivers();
    println!("{} driver(s) registered:", drivers.len());
    for d in drivers {
        let caps = d.capabilities();
        let schemes = d.supported_schemes().join(", ");
        println!();
        println!("- {}", d.name());
        println!("    schemes:           {schemes}");
        println!("    read/write:        {}", read_write(&caps));
        println!("    bulk_load:         {}", caps.bulk_load);
        println!("    random_access:     {}", caps.random_access);
        println!("    blob:              {}", caps.supports_blob);
        println!(
            "    decimal:           {}{}",
            caps.supports_decimal,
            match caps.max_decimal_precision {
                Some(p) => format!(" (max precision {p})"),
                None => String::new(),
            }
        );
        println!("    timestamp_tz:      {}", caps.supports_timestamp_tz);
        println!("    string_encoding:   {}", encoding_label(&caps));
    }
}

fn print_json(pretty: bool) -> Result<()> {
    let drivers = registry::all_drivers();
    let entries: Vec<DriverEntry> = drivers
        .iter()
        .map(|d| DriverEntry {
            name: d.name().to_string(),
            schemes: d.supported_schemes().to_vec(),
            capabilities: d.capabilities(),
        })
        .collect();
    println!("{}", serialize_json(&entries, pretty, "drivers")?);
    Ok(())
}

#[derive(Serialize)]
struct DriverEntry {
    name: String,
    schemes: Vec<&'static str>,
    capabilities: Capabilities,
}

fn read_write(c: &Capabilities) -> &'static str {
    match (c.read, c.write) {
        (true, true) => "read+write",
        (true, false) => "read-only",
        (false, true) => "write-only",
        (false, false) => "(neither)",
    }
}

fn encoding_label(c: &Capabilities) -> String {
    match c.string_encoding {
        // `Configurable` は第 1 要素を既定として扱う規約。
        StringEncoding::Fixed(s) => format!("fixed: {s}"),
        StringEncoding::Configurable(list) => format!("configurable: {}", list.join(", ")),
    }
}
