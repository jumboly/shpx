//! `shpx schema <src>` の実装。
//!
//! 入力ファイルの Arrow スキーマを JSON 化して標準出力へ書き出す。
//! arrow-schema 自体は serde 直シリアライズ可能ではないので、
//! `data_type` は `format!("{:?}", _)` で文字列化する。
//! geometry 列の `shpx:geometry` メタは JSON 値としてパース展開する。

use std::collections::HashMap;

use arrow_schema::{Field, Schema};
use serde_json::{Map, Value};
use shpx_core::{schema::GEOMETRY_META_KEY, Result};

use crate::cli::SchemaArgs;
use crate::commands::open_reader_for;

pub fn run(args: SchemaArgs) -> Result<()> {
    let (driver, reader) =
        open_reader_for(args.src.as_str(), args.src_crs.as_deref(), args.encoding)?;
    let schema = reader.schema();
    let value = schema_to_json(driver.name(), &schema);

    let s = if args.pretty {
        serde_json::to_string_pretty(&value)
    } else {
        serde_json::to_string(&value)
    }
    .map_err(|e| shpx_core::Error::Format(format!("schema json serialize failed: {e}")))?;
    println!("{s}");
    Ok(())
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
