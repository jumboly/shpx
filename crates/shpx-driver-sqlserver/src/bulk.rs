//! Arrow `RecordBatch` の各行を tiberius `TokenRow` に詰めるエンコーダ。
//!
//! geometry 列は WKB と SRID を 2 つの追加列として行末に詰める。staging テーブル側の
//! `[shpx_geom_wkb] varbinary(max)`, `[shpx_geom_srid] int` の 2 列に対応する。
//!
//! 案 B (DESIGN.md L.219-) の前提として、geometry/geography UDT の直接 bind は
//! tiberius が許さないため、ここでは行末で WKB+SRID に展開した形のみを生成する。

use std::borrow::Cow;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
    },
    Array, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef};
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Utc};
use shpx_core::{Error, Result};
use tiberius::{numeric::Numeric, ColumnData, IntoSql, TokenRow};

use crate::util::{driver_msg, primitive, timestamp_to_nanos};

/// 1 行 (`row` 番目) の Arrow セルを `TokenRow<'static>` に展開する。
///
/// 順序は staging テーブルのスキーマ:
/// 1. `attr_indices` の各属性列を順に
/// 2. `[shpx_geom_wkb]` (varbinary, WKB)
/// 3. `[shpx_geom_srid]` (int, SRID)
pub(crate) fn encode_row(
    schema: &SchemaRef,
    batch: &RecordBatch,
    attr_indices: &[usize],
    geom_index: usize,
    row: usize,
    srid: i32,
) -> Result<TokenRow<'static>> {
    let mut tr = TokenRow::with_capacity(attr_indices.len() + 2);
    for &col in attr_indices {
        let field = schema.field(col);
        let array: &dyn Array = batch.column(col).as_ref();
        let is_null = array.is_null(row);
        tr.push(arrow_to_column_data(
            field.data_type(),
            array,
            row,
            is_null,
            field.name(),
        )?);
    }

    let geom_array = batch.column(geom_index);
    let bb = geom_array.as_binary::<i32>();
    if bb.is_null(row) {
        tr.push(ColumnData::Binary(None));
    } else {
        tr.push(ColumnData::Binary(Some(Cow::Owned(bb.value(row).to_vec()))));
    }
    tr.push(ColumnData::I32(Some(srid)));

    Ok(tr)
}

#[allow(clippy::too_many_lines)]
fn arrow_to_column_data(
    dt: &DataType,
    array: &dyn Array,
    row: usize,
    is_null: bool,
    name: &str,
) -> Result<ColumnData<'static>> {
    Ok(match dt {
        DataType::Boolean => {
            if is_null {
                ColumnData::Bit(None)
            } else {
                ColumnData::Bit(Some(array.as_boolean().value(row)))
            }
        }
        DataType::Int16 => primitive_or_null::<Int16Type>(array, row, is_null, ColumnData::I16),
        DataType::Int32 => primitive_or_null::<Int32Type>(array, row, is_null, ColumnData::I32),
        DataType::Int64 => primitive_or_null::<Int64Type>(array, row, is_null, ColumnData::I64),
        DataType::Float32 => primitive_or_null::<Float32Type>(array, row, is_null, ColumnData::F32),
        DataType::Float64 => primitive_or_null::<Float64Type>(array, row, is_null, ColumnData::F64),
        DataType::Utf8 => {
            if is_null {
                ColumnData::String(None)
            } else {
                let s = array.as_string::<i32>().value(row).to_string();
                ColumnData::String(Some(Cow::Owned(s)))
            }
        }
        DataType::LargeUtf8 => {
            if is_null {
                ColumnData::String(None)
            } else {
                let s = array.as_string::<i64>().value(row).to_string();
                ColumnData::String(Some(Cow::Owned(s)))
            }
        }
        DataType::Binary => {
            if is_null {
                ColumnData::Binary(None)
            } else {
                let b = array.as_binary::<i32>().value(row).to_vec();
                ColumnData::Binary(Some(Cow::Owned(b)))
            }
        }
        DataType::LargeBinary => {
            if is_null {
                ColumnData::Binary(None)
            } else {
                let b = array.as_binary::<i64>().value(row).to_vec();
                ColumnData::Binary(Some(Cow::Owned(b)))
            }
        }
        DataType::Date32 => {
            // tiberius の `IntoSql for NaiveDate` 経由。100ns 解像度や AD 0001 起点の
            // 日数換算は tiberius 側に集約されている。
            if is_null {
                Option::<NaiveDate>::None.into_sql()
            } else {
                let days = primitive::<Date32Type>(array, row);
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
                let d = epoch
                    .checked_add_signed(Duration::days(i64::from(days)))
                    .ok_or_else(|| driver_msg(format!("column `{name}`: date overflow")))?;
                Some(d).into_sql()
            }
        }
        DataType::Timestamp(unit, None) => {
            if is_null {
                Option::<NaiveDateTime>::None.into_sql()
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let dt = nanos_to_utc(nanos, name)?.naive_utc();
                Some(dt).into_sql()
            }
        }
        DataType::Timestamp(unit, Some(_)) => {
            // tiberius の `IntoSql for DateTime<Utc>` は `ColumnData::DateTime2` を返してしまうため、
            // `datetimeoffset` 列に書くには `DateTime<FixedOffset>` 経由で `DateTimeOffset` を作らせる。
            if is_null {
                Option::<DateTime<FixedOffset>>::None.into_sql()
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let utc = nanos_to_utc(nanos, name)?;
                let zero = FixedOffset::east_opt(0).expect("0 offset");
                Some(utc.with_timezone(&zero)).into_sql()
            }
        }
        DataType::Decimal128(_p, s) => {
            if is_null {
                ColumnData::Numeric(None)
            } else {
                let i = primitive::<Decimal128Type>(array, row);
                let scale = u8::try_from(*s).map_err(|_| {
                    driver_msg(format!("column `{name}`: invalid Decimal128 scale {s}"))
                })?;
                ColumnData::Numeric(Some(Numeric::new_with_scale(i, scale)))
            }
        }
        other => {
            return Err(Error::Schema(format!(
                "column `{name}`: unsupported Arrow type for SQL Server bulk: {other:?}"
            )));
        }
    })
}

fn primitive_or_null<T: arrow_array::ArrowPrimitiveType>(
    array: &dyn Array,
    row: usize,
    is_null: bool,
    wrap: fn(Option<T::Native>) -> ColumnData<'static>,
) -> ColumnData<'static> {
    wrap(if is_null {
        None
    } else {
        Some(primitive::<T>(array, row))
    })
}

fn nanos_to_utc(nanos: i64, name: &str) -> Result<DateTime<Utc>> {
    let secs = nanos.div_euclid(1_000_000_000);
    let nanos_part = nanos.rem_euclid(1_000_000_000);
    let nanos_u = u32::try_from(nanos_part)
        .map_err(|_| driver_msg(format!("column `{name}`: timestamp nanos overflow")))?;
    Utc.timestamp_opt(secs, nanos_u)
        .single()
        .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp overflow")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::{
        builder::{BinaryBuilder, Int32Builder, StringBuilder},
        ArrayRef,
    };
    use arrow_schema::{Field, Schema};
    use std::sync::Arc;

    fn sample_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("geom", DataType::Binary, true),
        ]))
    }

    #[test]
    fn encode_row_packs_attr_geom_srid_in_order() {
        let schema = sample_schema();
        let mut id_b = Int32Builder::new();
        id_b.append_value(42);
        let mut name_b = StringBuilder::new();
        name_b.append_value("hello");
        let mut geom_b = BinaryBuilder::new();
        geom_b.append_value([0x01, 0x02, 0x03]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(id_b.finish()),
            Arc::new(name_b.finish()),
            Arc::new(geom_b.finish()),
        ];
        let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

        let row = encode_row(&schema, &batch, &[0, 1], 2, 0, 4326).unwrap();
        let cells: Vec<&ColumnData<'_>> = row.iter().collect();
        assert_eq!(cells.len(), 4);
        assert!(matches!(cells[0], ColumnData::I32(Some(42))));
        assert!(matches!(cells[1], ColumnData::String(Some(_))));
        assert!(matches!(cells[2], ColumnData::Binary(Some(_))));
        assert!(matches!(cells[3], ColumnData::I32(Some(4326))));
    }

    #[test]
    fn encode_row_handles_null_attr_and_geom() {
        let schema = sample_schema();
        let mut id_b = Int32Builder::new();
        id_b.append_value(1);
        let mut name_b = StringBuilder::new();
        name_b.append_null();
        let mut geom_b = BinaryBuilder::new();
        geom_b.append_null();
        let cols: Vec<ArrayRef> = vec![
            Arc::new(id_b.finish()),
            Arc::new(name_b.finish()),
            Arc::new(geom_b.finish()),
        ];
        let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

        let row = encode_row(&schema, &batch, &[0, 1], 2, 0, 0).unwrap();
        let cells: Vec<&ColumnData<'_>> = row.iter().collect();
        assert!(matches!(cells[1], ColumnData::String(None)));
        assert!(matches!(cells[2], ColumnData::Binary(None)));
    }
}
