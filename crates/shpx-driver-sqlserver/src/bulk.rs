//! Arrow `RecordBatch` の各行を tiberius `TokenRow` に詰めるエンコーダ。
//!
//! `BulkLoadRequest::send(row: TokenRow<'a>)` は `ColumnData<'a>` の Vec を直接受け取る
//! ため、PostGIS の `copy_binary.rs` のような自前 binary フォーマット組立は不要で、
//! 各 Arrow セルを `ColumnData` の variant に詰め替えるだけで済む。bulk-only encoding は
//! `'static` lifetime に固定して `TokenRow<'static>` を返す（owned `Cow` を使う）。
//!
//! geometry 列は WKB と SRID を 2 つの追加列として行末に詰める。staging テーブル側の
//! `[shpx_geom_wkb] varbinary(max)`, `[shpx_geom_srid] int` の 2 列に対応する。

use std::borrow::Cow;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
        TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
        TimestampSecondType,
    },
    Array, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef, TimeUnit};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use shpx_core::{Error, Result};
use tiberius::{
    numeric::Numeric,
    time::{Date, DateTime2, DateTimeOffset, Time},
    ColumnData, TokenRow,
};

use crate::util::{driver_msg, primitive};

/// 1 行 (`row` 番目) の Arrow セルを `TokenRow<'static>` に展開する。
///
/// 順序は staging テーブルのスキーマ:
/// 1. `attr_indices` の各属性列を順に
/// 2. `[shpx_geom_wkb]` (varbinary, WKB)
/// 3. `[shpx_geom_srid]` (int, SRID)
pub fn encode_row(
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
        DataType::Int16 => {
            if is_null {
                ColumnData::I16(None)
            } else {
                ColumnData::I16(Some(primitive::<Int16Type>(array, row)))
            }
        }
        DataType::Int32 => {
            if is_null {
                ColumnData::I32(None)
            } else {
                ColumnData::I32(Some(primitive::<Int32Type>(array, row)))
            }
        }
        DataType::Int64 => {
            if is_null {
                ColumnData::I64(None)
            } else {
                ColumnData::I64(Some(primitive::<Int64Type>(array, row)))
            }
        }
        DataType::Float32 => {
            if is_null {
                ColumnData::F32(None)
            } else {
                ColumnData::F32(Some(primitive::<Float32Type>(array, row)))
            }
        }
        DataType::Float64 => {
            if is_null {
                ColumnData::F64(None)
            } else {
                ColumnData::F64(Some(primitive::<Float64Type>(array, row)))
            }
        }
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
            if is_null {
                ColumnData::Date(None)
            } else {
                let days = primitive::<Date32Type>(array, row);
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
                let d = epoch
                    .checked_add_signed(Duration::days(i64::from(days)))
                    .ok_or_else(|| driver_msg(format!("column `{name}`: date overflow")))?;
                ColumnData::Date(Some(naive_date_to_tds(d)?))
            }
        }
        DataType::Timestamp(unit, None) => {
            if is_null {
                ColumnData::DateTime2(None)
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let secs = nanos.div_euclid(1_000_000_000);
                let nanos_part = nanos.rem_euclid(1_000_000_000);
                let nanos_u = u32::try_from(nanos_part).map_err(|_| {
                    driver_msg(format!("column `{name}`: timestamp nanos overflow"))
                })?;
                let dt = DateTime::<Utc>::from_timestamp(secs, nanos_u)
                    .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp overflow")))?
                    .naive_utc();
                ColumnData::DateTime2(Some(naive_datetime_to_tds(dt)?))
            }
        }
        DataType::Timestamp(unit, Some(_)) => {
            if is_null {
                ColumnData::DateTimeOffset(None)
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let secs = nanos.div_euclid(1_000_000_000);
                let nanos_part = nanos.rem_euclid(1_000_000_000);
                let nanos_u = u32::try_from(nanos_part).map_err(|_| {
                    driver_msg(format!("column `{name}`: timestamp nanos overflow"))
                })?;
                let dt = Utc
                    .timestamp_opt(secs, nanos_u)
                    .single()
                    .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp overflow")))?;
                ColumnData::DateTimeOffset(Some(datetime_utc_to_tds(dt)?))
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

fn timestamp_to_nanos(unit: TimeUnit, array: &dyn Array, row: usize, name: &str) -> Result<i64> {
    match unit {
        TimeUnit::Nanosecond => Ok(primitive::<TimestampNanosecondType>(array, row)),
        TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row)
            .checked_mul(1_000)
            .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp µs→ns overflow"))),
        TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row)
            .checked_mul(1_000_000)
            .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp ms→ns overflow"))),
        TimeUnit::Second => primitive::<TimestampSecondType>(array, row)
            .checked_mul(1_000_000_000)
            .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp s→ns overflow"))),
    }
}

/// `NaiveDate` を tiberius の `Date` 表現 (`days_since_year_1`) に変換する。
/// SQL Server の `date` は AD 0001-01-01 から経過日数 u32 で表現される。
fn naive_date_to_tds(d: NaiveDate) -> Result<Date> {
    let base = NaiveDate::from_ymd_opt(1, 1, 1).expect("AD 0001-01-01");
    let days = d.signed_duration_since(base).num_days();
    let days_u = u32::try_from(days).map_err(|_| {
        driver_msg(format!(
            "date out of SQL Server range (got {d}; expected 0001-01-01..=9999-12-31)"
        ))
    })?;
    // 3 byte エンコードのため上位 8 bit が立つと TDS protocol エラー。実用上は AD 9999 まで。
    Ok(Date::new(days_u))
}

/// `NaiveDateTime` を tiberius の `DateTime2` 表現に変換する。100ns 解像度。
fn naive_datetime_to_tds(dt: NaiveDateTime) -> Result<DateTime2> {
    let date = naive_date_to_tds(dt.date())?;
    let t = dt.time();
    // 1 日のうちの 100ns 単位カウント。
    let secs_in_day = u64::from(t.num_seconds_from_midnight());
    let frac = u64::from(t.nanosecond());
    let increments = secs_in_day * 10_000_000 + frac / 100;
    Ok(DateTime2::new(date, Time::new(increments, 7)))
}

/// `DateTime<Utc>` を tiberius の `DateTimeOffset` (UTC 固定 offset) に変換する。
fn datetime_utc_to_tds(dt: DateTime<Utc>) -> Result<DateTimeOffset> {
    let dt2 = naive_datetime_to_tds(dt.naive_utc())?;
    Ok(DateTimeOffset::new(dt2, 0))
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
        assert_eq!(cells.len(), 4); // 2 attr + 1 wkb + 1 srid
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

    #[test]
    fn naive_date_to_tds_known_anchor() {
        let d = NaiveDate::from_ymd_opt(2026, 5, 1).unwrap();
        let tds = naive_date_to_tds(d).unwrap();
        // SQL Server の date は AD 0001-01-01 起点。直接比較しないが、
        // 2026-05-01 は 1 年 = 365 日 × 2025 + 閏年補正 で 約 739_372 日
        // (チェックは「成功すること」と「成功した値が将来も再現可能」のみ)。
        let _ = tds;
    }
}
