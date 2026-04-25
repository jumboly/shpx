//! `shpx drivers` の実装。登録 driver と capabilities を一覧表示する。

use shpx_core::{Capabilities, StringEncoding};

use crate::registry;

pub fn run() {
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
