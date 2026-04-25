//! shpx CLI のエントリポイント。

mod registry;

fn main() {
    let names: Vec<&'static str> = registry::all_drivers().iter().map(|d| d.name()).collect();
    eprintln!("shpx (v0.1 skeleton). drivers = {names:?}");
}
