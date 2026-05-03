//! `shpx schema <src>` の実装。
//!
//! 入力ファイルの Arrow スキーマを `--format=json` (既定) または
//! `--format=text` で標準出力に書き出す。`arrow_schema::Schema` 自体は serde
//! 直シリアライズ可能ではないので、`data_type` は `format!("{:?}", _)` で
//! 文字列化する。geometry 列の `shpx:geometry` メタは JSON 値として展開する
//! (text 経路では長さが 100 文字を超えると省略される)。

use std::collections::HashMap;

use arrow_schema::{Field, Schema};
use serde_json::{Map, Value};
use shpx_core::{schema::GEOMETRY_META_KEY, Result};

use crate::cli::{OutputFormatArg, SchemaArgs};
use crate::commands::{open_reader_for, serialize_json};

const METADATA_LINE_MAX: usize = 100;

pub fn run(args: SchemaArgs) -> Result<()> {
    let (driver, reader) =
        open_reader_for(args.src.as_str(), args.src_crs.as_deref(), args.encoding)?;
    let schema = reader.schema();

    match args.format {
        OutputFormatArg::Json => {
            let value = schema_to_json(driver.name(), &schema);
            println!("{}", serialize_json(&value, args.pretty, "schema")?);
            Ok(())
        }
        OutputFormatArg::Text => {
            print_text(driver.name(), &schema);
            Ok(())
        }
    }
}

/// 人間可読の表形式。列幅は `name` / `data_type` の最大値に合わせて整列する。
/// `shpx:geometry` メタは長くなりがちなので 1 行 `METADATA_LINE_MAX` 文字で
/// 省略表示し、完全な値は `--format=json` で取得する想定。
fn print_text(driver_name: &str, schema: &Schema) {
    println!("driver: {driver_name}");

    let fields = schema.fields();
    println!("fields ({}):", fields.len());

    // `Debug` 文字列化は alloc を伴うので幅算出と print で再利用する。
    let dtype_strs: Vec<String> = fields
        .iter()
        .map(|f| format!("{:?}", f.data_type()))
        .collect();

    let name_w = fields
        .iter()
        .map(|f| f.name().chars().count())
        .max()
        .unwrap_or(0)
        .max("name".len());
    let dtype_w = dtype_strs
        .iter()
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(0)
        .max("data_type".len());

    for (f, dtype_str) in fields.iter().zip(&dtype_strs) {
        let nullable = if f.is_nullable() { "yes" } else { "no" };
        println!(
            "  {:<name_w$}  {:<dtype_w$}  nullable={nullable}",
            f.name(),
            dtype_str,
            name_w = name_w,
            dtype_w = dtype_w,
        );
        for (k, v) in f.metadata() {
            let v_short = abbreviate(v, METADATA_LINE_MAX);
            println!("      {k}: {v_short}");
        }
    }

    if !schema.metadata().is_empty() {
        println!("metadata:");
        for (k, v) in schema.metadata() {
            println!("  {k}: {v}");
        }
    }
}

fn abbreviate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{head}...")
    }
}

fn schema_to_json(driver_name: &str, schema: &Schema) -> Value {
    let mut top = Map::new();
    top.insert("driver".to_string(), Value::String(driver_name.to_string()));
    top.insert(
        "fields".to_string(),
        Value::Array(schema.fields().iter().map(|f| field_to_json(f)).collect()),
    );
    if !schema.metadata().is_empty() {
        top.insert(
            "metadata".to_string(),
            Value::Object(string_map_to_json(schema.metadata())),
        );
    }
    Value::Object(top)
}

fn field_to_json(f: &Field) -> Value {
    let mut obj = Map::new();
    obj.insert("name".to_string(), Value::String(f.name().clone()));
    obj.insert(
        "data_type".to_string(),
        Value::String(format!("{:?}", f.data_type())),
    );
    obj.insert("nullable".to_string(), Value::Bool(f.is_nullable()));
    if !f.metadata().is_empty() {
        obj.insert(
            "metadata".to_string(),
            Value::Object(field_metadata_to_json(f.metadata())),
        );
    }
    Value::Object(obj)
}

/// `shpx:geometry` キーの値は埋め込み JSON テキストなので構造体として展開する。
/// 他キーは普通の文字列扱い。パース失敗時は raw 文字列を残して診断性を優先する。
fn field_metadata_to_json(meta: &HashMap<String, String>) -> Map<String, Value> {
    meta.iter()
        .map(|(k, v)| {
            let value = if k == GEOMETRY_META_KEY {
                serde_json::from_str::<Value>(v).unwrap_or_else(|_| Value::String(v.clone()))
            } else {
                Value::String(v.clone())
            };
            (k.clone(), value)
        })
        .collect()
}

fn string_map_to_json(meta: &HashMap<String, String>) -> Map<String, Value> {
    meta.iter()
        .map(|(k, v)| (k.clone(), Value::String(v.clone())))
        .collect()
}
