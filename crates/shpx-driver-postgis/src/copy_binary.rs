//! PostgreSQL binary COPY format エンコーダ。
//!
//! `COPY <table> FROM STDIN BINARY` のフレーム形式を自前で組み立てる。
//! `tokio_postgres` 標準の `ToSql` 実装と同じバイト列を出すが、prepared INSERT
//! の `Box<dyn ToSql>` ヒープ確保や per-row ラウンドトリップが不要になるため、
//! 大量行投入で大きく速くなる経路を提供する。
//!
//! フォーマット概要（PostgreSQL 公式ドキュメント "Binary Format" 節）:
//!
//! - File header (19 bytes): `PGCOPY\n\xff\r\n\0` (11) + `flags i32 BE` (0) + `header ext i32 BE` (0)
//! - Tuple: `field_count i16 BE` + 各 field `len i32 BE` + payload。`len = -1` は NULL。
//! - Trailer: `i16 BE = -1`
//!
//! 数値型は全て big-endian。`numeric` のみ NBASE=10000 表現で別途エンコード（[`PgNumeric`]）。
//!
//! Decimal128 / numeric の同じバイト列を batch / bulk 両経路で使い回すため、
//! `PgNumeric` は `tokio_postgres::types::ToSql` / `FromSql` も実装する。

use std::error::Error as StdError;

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
        TimestampMicrosecondType, TimestampMillisecondType, TimestampNanosecondType,
        TimestampSecondType,
    },
    Array, RecordBatch,
};
use arrow_schema::{DataType, Field, SchemaRef, TimeUnit};
use bytes::{BufMut, BytesMut};
use postgres_types::{accepts, FromSql, IsNull, ToSql, Type as PgType};
use shpx_core::{Error, Result};
use shpx_geom::ewkb;

use crate::util::{driver_msg, primitive};

/// COPY BINARY のファイルヘッダ。19 byte 固定。
///
/// - `PGCOPY\n\xff\r\n\0` (11 byte signature)
/// - `flags` (4 byte BE i32, 値 0)
/// - `header extension area length` (4 byte BE i32, 値 0)
pub const COPY_HEADER: &[u8] = b"PGCOPY\n\xff\r\n\0\x00\x00\x00\x00\x00\x00\x00\x00";

/// COPY BINARY のトレーラ。`i16 BE = -1`。
pub const COPY_TRAILER: [u8; 2] = [0xff, 0xff];

/// 2000-01-01 - 1970-01-01 の日数（PG epoch オフセット）。
const PG_EPOCH_DAYS_FROM_UNIX: i32 = 10_957;

/// 2000-01-01 00:00:00 UTC - 1970-01-01 00:00:00 UTC のマイクロ秒。
const PG_EPOCH_MICROS_FROM_UNIX: i64 = 946_684_800_000_000;

/// 行単位のエンコード状態。`PostgisWriter` が schema / geom 列 / SRID を渡して構築し、
/// batch のループ内で `encode_row` を呼ぶ。
pub struct BulkRowEncoder {
    schema: SchemaRef,
    /// CREATE TABLE / COPY 列リストと一致する出力列順 = attr_indices + [geom_index]。
    field_order: Vec<usize>,
    geom_index: usize,
    srid: i32,
}

/// COPY BINARY のヘッダ 19 byte を `out` に書く。`bulk_write` の最初に 1 度だけ呼ぶ。
pub fn write_copy_header(out: &mut BytesMut) {
    out.extend_from_slice(COPY_HEADER);
}

/// COPY BINARY のトレーラ `i16 BE = -1` を `out` に書く。`bulk_write` の最後に 1 度だけ呼ぶ。
pub fn write_copy_trailer(out: &mut BytesMut) {
    out.extend_from_slice(&COPY_TRAILER);
}

impl BulkRowEncoder {
    /// `attr_indices` + `geom_index` の順で COPY する。型チェックは `encode_row` 内の
    /// match で行う（事前パスは冗長で同じ列挙を 2 箇所に維持する負担になるため省略）。
    pub fn new(
        schema: SchemaRef,
        attr_indices: Vec<usize>,
        geom_index: usize,
        srid: i32,
    ) -> Result<Self> {
        let mut field_order = attr_indices;
        field_order.push(geom_index);
        Ok(Self {
            schema,
            field_order,
            geom_index,
            srid,
        })
    }

    /// 1 行分を `out` に append する。
    pub fn encode_row(&self, batch: &RecordBatch, row: usize, out: &mut BytesMut) -> Result<()> {
        let field_count: i16 = i16::try_from(self.field_order.len())
            .map_err(|_| driver_msg("COPY field_count exceeds i16"))?;
        out.put_i16(field_count);
        for &idx in &self.field_order {
            if idx == self.geom_index {
                encode_geometry(batch, idx, row, self.srid, out)?;
            } else {
                let field = self.schema.field(idx);
                encode_field(field, batch, idx, row, out)?;
            }
        }
        Ok(())
    }
}

fn encode_field(
    field: &Field,
    batch: &RecordBatch,
    col: usize,
    row: usize,
    out: &mut BytesMut,
) -> Result<()> {
    let array = batch.column(col).as_ref();
    if array.is_null(row) {
        encode_null(out);
        return Ok(());
    }
    match field.data_type() {
        DataType::Boolean => {
            let v = array.as_boolean().value(row);
            put_field(out, &[u8::from(v)]);
        }
        DataType::Int16 => {
            let v = primitive::<Int16Type>(array, row);
            put_field_i16(out, v);
        }
        DataType::Int32 => {
            let v = primitive::<Int32Type>(array, row);
            put_field_i32(out, v);
        }
        DataType::Int64 => {
            let v = primitive::<Int64Type>(array, row);
            put_field_i64(out, v);
        }
        DataType::Float32 => {
            let v = primitive::<Float32Type>(array, row);
            let bytes = v.to_bits().to_be_bytes();
            put_field(out, &bytes);
        }
        DataType::Float64 => {
            let v = primitive::<Float64Type>(array, row);
            let bytes = v.to_bits().to_be_bytes();
            put_field(out, &bytes);
        }
        DataType::Utf8 => {
            let s = array.as_string::<i32>().value(row);
            put_field(out, s.as_bytes());
        }
        DataType::LargeUtf8 => {
            let s = array.as_string::<i64>().value(row);
            put_field(out, s.as_bytes());
        }
        DataType::Binary => {
            let b = array.as_binary::<i32>().value(row);
            put_field(out, b);
        }
        DataType::LargeBinary => {
            let b = array.as_binary::<i64>().value(row);
            put_field(out, b);
        }
        DataType::Date32 => {
            let days_unix = primitive::<Date32Type>(array, row);
            let pg_days = days_unix
                .checked_sub(PG_EPOCH_DAYS_FROM_UNIX)
                .ok_or_else(|| driver_msg(format!("field `{}`: date overflow", field.name())))?;
            put_field_i32(out, pg_days);
        }
        DataType::Timestamp(unit, _tz) => {
            let micros_unix = ts_to_unix_micros(array, row, *unit, field.name())?;
            let pg_micros = micros_unix
                .checked_sub(PG_EPOCH_MICROS_FROM_UNIX)
                .ok_or_else(|| {
                    driver_msg(format!("field `{}`: timestamp overflow", field.name()))
                })?;
            put_field_i64(out, pg_micros);
        }
        DataType::Decimal128(_p, s) => {
            let v: i128 = primitive::<Decimal128Type>(array, row);
            let scale = u8::try_from(*s).map_err(|_| {
                driver_msg(format!(
                    "field `{}`: Decimal128 scale {} not supported (must be 0..=38)",
                    field.name(),
                    s
                ))
            })?;
            let pgn = PgNumeric::from_i128_scale(v, scale);
            put_field_pgnumeric(out, &pgn);
        }
        other => {
            return Err(Error::Schema(format!(
                "field `{}`: unsupported Arrow type for PostGIS bulk writer: {other:?}",
                field.name()
            )));
        }
    }
    Ok(())
}

fn encode_geometry(
    batch: &RecordBatch,
    col: usize,
    row: usize,
    srid: i32,
    out: &mut BytesMut,
) -> Result<()> {
    let arr = batch.column(col).as_binary::<i32>();
    if arr.is_null(row) {
        encode_null(out);
        return Ok(());
    }
    let wkb = arr.value(row);
    let ewkb_bytes = ewkb::encode_with_srid(wkb, srid)?;
    put_field(out, &ewkb_bytes);
    Ok(())
}

fn encode_null(out: &mut BytesMut) {
    out.put_i32(-1);
}

fn put_field(out: &mut BytesMut, payload: &[u8]) {
    let len = i32::try_from(payload.len()).expect("field length fits i32");
    out.put_i32(len);
    out.extend_from_slice(payload);
}

fn put_field_i16(out: &mut BytesMut, v: i16) {
    out.put_i32(2);
    out.put_i16(v);
}

fn put_field_i32(out: &mut BytesMut, v: i32) {
    out.put_i32(4);
    out.put_i32(v);
}

fn put_field_i64(out: &mut BytesMut, v: i64) {
    out.put_i32(8);
    out.put_i64(v);
}

fn put_field_pgnumeric(out: &mut BytesMut, n: &PgNumeric) {
    let len = 8 + 2 * i32::try_from(n.digits.len()).expect("digit count fits i32");
    out.put_i32(len);
    n.write_payload(out);
}

fn ts_to_unix_micros(array: &dyn Array, row: usize, unit: TimeUnit, name: &str) -> Result<i64> {
    Ok(match unit {
        TimeUnit::Second => primitive::<TimestampSecondType>(array, row)
            .checked_mul(1_000_000)
            .ok_or_else(|| driver_msg(format!("field `{name}`: timestamp seconds overflow")))?,
        TimeUnit::Millisecond => primitive::<TimestampMillisecondType>(array, row)
            .checked_mul(1_000)
            .ok_or_else(|| driver_msg(format!("field `{name}`: timestamp ms overflow")))?,
        TimeUnit::Microsecond => primitive::<TimestampMicrosecondType>(array, row),
        TimeUnit::Nanosecond => primitive::<TimestampNanosecondType>(array, row) / 1_000,
    })
}

// ---------------------------------------------------------------------------
// PgNumeric: Arrow Decimal128 ↔ PostgreSQL numeric の binary 表現
// ---------------------------------------------------------------------------

const NUMERIC_POS: u16 = 0x0000;
const NUMERIC_NEG: u16 = 0x4000;
const NUMERIC_NAN: u16 = 0xC000;

/// PostgreSQL `numeric` 型の binary 表現。NBASE=10000 の桁配列で値を保持する。
///
/// - `digits[0]` が最上位非ゼロ NBASE 桁（4 進数桁）。`digits.len() == 0` は値 0。
/// - `weight` は `digits[0]` の桁位置（10000^weight）。
/// - `sign` は [`NUMERIC_POS`] / [`NUMERIC_NEG`] / [`NUMERIC_NAN`]。
/// - `dscale` は表示用の小数点桁数（Arrow Decimal128(p, s) の s に相当）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PgNumeric {
    pub digits: Vec<i16>,
    pub weight: i16,
    pub sign: u16,
    pub dscale: u16,
}

/// PG numeric 1 桁の重み (NBASE = 10000)。
const NBASE: i128 = 10_000;

impl PgNumeric {
    /// Arrow `Decimal128(p, s)` の生 `i128` 値と scale から構築する。
    ///
    /// `value` は scale 込みの整数表現（つまり `value = real_value * 10^scale`）。
    /// `scale` は 0..=38 を想定する。Decimal128(38, _) の有効値域 (±10^38-1) は
    /// i128 の値域 (±2^127 ≈ ±1.7e38) に収まるため、`value.unsigned_abs()` で
    /// オーバーフローしない。
    #[must_use]
    pub fn from_i128_scale(value: i128, scale: u8) -> Self {
        let dscale = u16::from(scale);
        if value == 0 {
            return Self {
                digits: Vec::new(),
                weight: 0,
                sign: NUMERIC_POS,
                dscale,
            };
        }
        let (sign, abs) = if value < 0 {
            (NUMERIC_NEG, value.unsigned_abs())
        } else {
            (
                NUMERIC_POS,
                u128::try_from(value).expect("non-negative i128"),
            )
        };
        Self::from_u128_scale(abs, scale, sign, dscale)
    }

    /// `i128` の絶対値 (`abs > 0`) を NBASE=10000 桁列に直接分解する。
    ///
    /// 文字列を経由せず `abs % NBASE` の繰り返しで下位桁から digit を取る。1 行ごとの heap
    /// 確保が `Vec<i16> digits` 1 個だけになる（cycle 2 の hot path 最適化）。
    ///
    /// 桁配置: `s = 4q + r` (0 ≤ r < 4) と書くと、`value × 10^s` の NBASE 表現は最下位 NBASE
    /// 桁が `pad_low = (4 - r) % 4` 桁の trailing zero を含む。よって最下位桁を「abs の下位 r 桁
    /// を上位 r 桁とする 4 桁」として取り出し、その後 abs の残り (= abs / 10^r) を NBASE で
    /// 割り続けて上位桁を順に取る。abs × 10^pad_low を作ると max Decimal128(38, _) で u128 を
    /// 超えるため、widening は使わず最下位桁を別経路で扱う。
    fn from_u128_scale(abs: u128, scale: u8, sign: u16, dscale: u16) -> Self {
        debug_assert!(abs > 0);
        let s = i32::from(scale);
        let r = u32::try_from(s.rem_euclid(4)).expect("s % 4 fits u32");
        let pad_low = if r == 0 { 0 } else { 4 - r };

        let mut digits: Vec<i16> = Vec::new();
        let mut n = abs;
        if pad_low > 0 {
            let p10 = u128::from(10u32.pow(r));
            let low_digits = u16::try_from(n % p10).expect("n % 10^r < 10^4");
            let last_chunk =
                i16::try_from(low_digits).expect("low < 10000 fits i16") * 10i16.pow(pad_low);
            digits.push(last_chunk);
            n /= p10;
        }
        let nbase = u128::try_from(NBASE).expect("NBASE positive");
        while n > 0 {
            let d = i16::try_from(n % nbase).expect("digit < NBASE fits i16");
            digits.push(d);
            n /= nbase;
        }
        digits.reverse();

        // 最下位 NBASE 桁の重み = -(s + pad_low) / 4。最上位は + (digits.len() - 1)。
        let pad_low_i32 = i32::try_from(pad_low).expect("pad_low ≤ 3 fits i32");
        let weight_low = -(s + pad_low_i32) / 4;
        let digits_len_i32 = i32::try_from(digits.len()).expect("digit count fits i32");
        let mut weight = weight_low + digits_len_i32 - 1;

        // 末尾の 0 桁をトリム（dscale が真値を保持するので情報損失なし）。
        while digits.last() == Some(&0) {
            digits.pop();
        }
        if digits.is_empty() {
            weight = 0;
        }

        let weight = i16::try_from(weight).expect("weight fits i16 for Decimal128");
        Self {
            digits,
            weight,
            sign,
            dscale,
        }
    }

    /// `i128` 値（scale 込み整数表現）に戻す。`target_scale` は呼び出し側の Arrow 列の scale。
    ///
    /// `dscale != target_scale` のときは `target_scale` 側に揃える（不足は 0 補完、過剰は切り捨て =
    /// ROUND_DOWN）。NaN は `Error::Schema` で停止する。
    ///
    /// 直接 `d_i × 10^e_i` を i128 で計算すると最上位 NBASE 桁の中間項が overflow する
    /// （例: max Decimal128(38, 10) では `9999 × 10^34` ≈ 10^38 で危険）。そこで rolling
    /// accumulator 方式: 左から右へ走査しながら直前の指数との差だけ acc を 10 倍して桁を
    /// 取り込み、ループ後に最後の指数だけ shift して exp=0 に揃える。
    pub fn to_i128_with_scale(&self, target_scale: u8) -> Result<i128> {
        if self.sign == NUMERIC_NAN {
            return Err(Error::Schema(
                "PostgreSQL numeric NaN cannot be represented as Decimal128".into(),
            ));
        }
        let s = i32::from(target_scale);
        let mut acc: i128 = 0;
        let mut prev_e: Option<i32> = None;
        let mut truncated = false;

        for (i, &d) in self.digits.iter().enumerate() {
            let idx = i32::try_from(i).expect("digit index fits i32");
            let e = (i32::from(self.weight) - idx) * 4 + s;
            if e < 0 {
                // target_scale より細かい桁。最初の負指数桁だけ truncate して足す。
                // 後続の桁は 10^(e-4), 10^(e-8), ... と急速に小さくなり、整数 floor への
                // 寄与は 1 未満（場合により carry でずれる可能性があるが、Decimal128 と
                // PG numeric の精度域では実用上問題にならないため cycle 2 ではこの扱い）。
                if let Some(pe) = prev_e {
                    if pe > 0 {
                        let pw = u32::try_from(pe).expect("positive exp fits u32");
                        let pow = 10i128.checked_pow(pw).ok_or_else(|| {
                            Error::Schema(format!(
                                "numeric exponent overflow at digit {i} (10^{pw})"
                            ))
                        })?;
                        acc = acc.checked_mul(pow).ok_or_else(|| {
                            Error::Schema(format!("numeric accumulator overflow at digit {i}"))
                        })?;
                    }
                }
                let neg_e = u32::try_from(-e).expect("negative exp magnitude fits u32");
                let div = 10i128.checked_pow(neg_e).ok_or_else(|| {
                    Error::Schema(format!("numeric truncation divisor overflow at digit {i}"))
                })?;
                let frac = i128::from(d) / div;
                acc = acc.checked_add(frac).ok_or_else(|| {
                    Error::Schema(format!("numeric truncation add overflow at digit {i}"))
                })?;
                truncated = true;
                break;
            }
            if let Some(pe) = prev_e {
                let shift = u32::try_from(pe - e).expect("monotone decreasing exp");
                let pow = 10i128.checked_pow(shift).ok_or_else(|| {
                    Error::Schema(format!("numeric shift overflow at digit {i} (10^{shift})"))
                })?;
                acc = acc.checked_mul(pow).ok_or_else(|| {
                    Error::Schema(format!("numeric accumulator overflow at digit {i}"))
                })?;
            }
            acc = acc
                .checked_add(i128::from(d))
                .ok_or_else(|| Error::Schema(format!("numeric digit add overflow at digit {i}")))?;
            prev_e = Some(e);
        }

        if !truncated {
            if let Some(pe) = prev_e {
                if pe > 0 {
                    let pw = u32::try_from(pe).expect("positive final exp fits u32");
                    let pow = 10i128.checked_pow(pw).ok_or_else(|| {
                        Error::Schema(format!("numeric final shift overflow (10^{pw})"))
                    })?;
                    acc = acc.checked_mul(pow).ok_or_else(|| {
                        Error::Schema("numeric final accumulator overflow".into())
                    })?;
                }
            }
        }

        if self.sign == NUMERIC_NEG {
            acc = acc.checked_neg().ok_or_else(|| {
                Error::Schema("numeric negation overflow (value is i128::MIN)".into())
            })?;
        }
        Ok(acc)
    }

    fn write_payload(&self, out: &mut BytesMut) {
        let ndigits = i16::try_from(self.digits.len()).expect("ndigits fits i16");
        out.put_i16(ndigits);
        out.put_i16(self.weight);
        out.put_u16(self.sign);
        out.put_u16(self.dscale);
        for d in &self.digits {
            out.put_i16(*d);
        }
    }
}

impl ToSql for PgNumeric {
    fn to_sql(
        &self,
        _ty: &PgType,
        out: &mut BytesMut,
    ) -> std::result::Result<IsNull, Box<dyn StdError + Sync + Send>> {
        self.write_payload(out);
        Ok(IsNull::No)
    }

    accepts!(NUMERIC);

    postgres_types::to_sql_checked!();
}

impl<'a> FromSql<'a> for PgNumeric {
    fn from_sql(
        _ty: &PgType,
        raw: &'a [u8],
    ) -> std::result::Result<Self, Box<dyn StdError + Sync + Send>> {
        if raw.len() < 8 {
            return Err("numeric payload too short".into());
        }
        let ndigits = u16::from_be_bytes([raw[0], raw[1]]) as usize;
        let weight = i16::from_be_bytes([raw[2], raw[3]]);
        let sign = u16::from_be_bytes([raw[4], raw[5]]);
        let dscale = u16::from_be_bytes([raw[6], raw[7]]);
        if raw.len() < 8 + ndigits * 2 {
            return Err("numeric payload truncated".into());
        }
        let mut digits = Vec::with_capacity(ndigits);
        for i in 0..ndigits {
            let off = 8 + i * 2;
            digits.push(i16::from_be_bytes([raw[off], raw[off + 1]]));
        }
        Ok(PgNumeric {
            digits,
            weight,
            sign,
            dscale,
        })
    }

    accepts!(NUMERIC);
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use arrow_array::{
        builder::{
            BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder,
            Float64Builder, Int16Builder, Int32Builder, Int64Builder, StringBuilder,
            TimestampMicrosecondBuilder,
        },
        ArrayRef, RecordBatch,
    };
    use arrow_schema::{Field, Schema as ASchema};
    use chrono::TimeZone;

    fn schema_of(fields: Vec<Field>) -> SchemaRef {
        Arc::new(ASchema::new(fields))
    }

    #[test]
    fn header_bytes_match_pg_spec() {
        // 11 byte signature + 4 byte flags + 4 byte ext = 19 byte
        assert_eq!(COPY_HEADER.len(), 19);
        assert_eq!(&COPY_HEADER[..11], b"PGCOPY\n\xff\r\n\0");
        assert_eq!(&COPY_HEADER[11..15], &[0, 0, 0, 0]);
        assert_eq!(&COPY_HEADER[15..19], &[0, 0, 0, 0]);
    }

    #[test]
    fn trailer_is_negative_one_be() {
        assert_eq!(COPY_TRAILER, [0xff, 0xff]);
    }

    #[test]
    fn pg_epoch_offsets() {
        // 1970-01-01 + 10957 days = 2000-01-01
        let nd = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap()
            + chrono::Duration::days(i64::from(PG_EPOCH_DAYS_FROM_UNIX));
        assert_eq!(nd, chrono::NaiveDate::from_ymd_opt(2000, 1, 1).unwrap());

        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(0, 0).unwrap()
            + chrono::Duration::microseconds(PG_EPOCH_MICROS_FROM_UNIX);
        assert_eq!(
            dt,
            chrono::Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap()
        );
    }

    fn encode_row_bytes(
        schema: SchemaRef,
        attr_indices: Vec<usize>,
        geom_index: usize,
        srid: i32,
        batch: &RecordBatch,
        row: usize,
    ) -> Vec<u8> {
        let enc = BulkRowEncoder::new(schema, attr_indices, geom_index, srid).unwrap();
        let mut out = BytesMut::new();
        enc.encode_row(batch, row, &mut out).unwrap();
        out.to_vec()
    }

    fn binary_column_with_value(value: &[u8]) -> ArrayRef {
        let mut b = BinaryBuilder::new();
        b.append_value(value);
        Arc::new(b.finish())
    }

    fn point_wkb_le() -> Vec<u8> {
        // POINT(1 2) little-endian WKB:
        // 01 (LE) 01000000 (type=1) 0000000000000000+0040 0000000000000000+4000
        let mut v = vec![0x01];
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&1.0f64.to_le_bytes());
        v.extend_from_slice(&2.0f64.to_le_bytes());
        v
    }

    #[test]
    fn encode_row_int32_and_geom() {
        // schema: [Int32, Binary geom]
        let s = schema_of(vec![
            Field::new("v", DataType::Int32, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut vb = Int32Builder::new();
        vb.append_value(42);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(vb.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 4326, &batch, 0);

        // field_count = 2 (i16 BE)
        assert_eq!(&bytes[0..2], &[0x00, 0x02]);
        // field 1: i32(4) + Int32(42) BE
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x04]);
        assert_eq!(&bytes[6..10], &[0x00, 0x00, 0x00, 0x2a]);
        // field 2: i32 length + EWKB body
        let len_bytes: [u8; 4] = bytes[10..14].try_into().unwrap();
        let len = usize::try_from(i32::from_be_bytes(len_bytes)).expect("non-negative len");
        assert_eq!(bytes.len(), 14 + len);
        // EWKB: byte 0 = 0x01 LE, type|SRID flag, srid=4326 i32 LE, then x,y f64 LE
        assert_eq!(bytes[14], 0x01);
        let typ = u32::from_le_bytes(bytes[15..19].try_into().unwrap());
        assert_eq!(typ, 1 | 0x2000_0000);
        let srid = i32::from_le_bytes(bytes[19..23].try_into().unwrap());
        assert_eq!(srid, 4326);
    }

    #[test]
    fn encode_row_handles_null_field() {
        let s = schema_of(vec![
            Field::new("v", DataType::Int32, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut vb = Int32Builder::new();
        vb.append_null();
        let cols: Vec<ArrayRef> = vec![
            Arc::new(vb.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 0, &batch, 0);
        // field_count = 2
        assert_eq!(&bytes[0..2], &[0x00, 0x02]);
        // field 1: length = -1 (NULL)
        assert_eq!(&bytes[2..6], &[0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn encode_row_null_geometry() {
        let s = schema_of(vec![
            Field::new("v", DataType::Int32, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut vb = Int32Builder::new();
        vb.append_value(1);
        let mut gb = BinaryBuilder::new();
        gb.append_null();
        let cols: Vec<ArrayRef> = vec![Arc::new(vb.finish()), Arc::new(gb.finish())];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 4326, &batch, 0);
        // last 4 bytes = i32 -1
        assert_eq!(&bytes[bytes.len() - 4..], &[0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn encode_row_bool_text_bytea() {
        let s = schema_of(vec![
            Field::new("b", DataType::Boolean, true),
            Field::new("t", DataType::Utf8, true),
            Field::new("blob", DataType::Binary, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut bb = BooleanBuilder::new();
        bb.append_value(true);
        let mut tb = StringBuilder::new();
        tb.append_value("abc");
        let mut blob_b = BinaryBuilder::new();
        blob_b.append_value([0xde, 0xad, 0xbe, 0xef]);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(bb.finish()),
            Arc::new(tb.finish()),
            Arc::new(blob_b.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0, 1, 2], 3, 0, &batch, 0);

        // bool (1 byte 0x01)
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x01]);
        assert_eq!(bytes[6], 0x01);
        // text "abc"
        assert_eq!(&bytes[7..11], &[0x00, 0x00, 0x00, 0x03]);
        assert_eq!(&bytes[11..14], b"abc");
        // bytea 4 byte
        assert_eq!(&bytes[14..18], &[0x00, 0x00, 0x00, 0x04]);
        assert_eq!(&bytes[18..22], &[0xde, 0xad, 0xbe, 0xef]);
    }

    #[test]
    fn encode_row_floats() {
        let s = schema_of(vec![
            Field::new("f4", DataType::Float32, true),
            Field::new("f8", DataType::Float64, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut a = Float32Builder::new();
        a.append_value(1.5);
        let mut b = Float64Builder::new();
        b.append_value(-1.25);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(a.finish()),
            Arc::new(b.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0, 1], 2, 0, &batch, 0);

        // f4: len 4 + BE bits
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x04]);
        let f4_bytes: [u8; 4] = bytes[6..10].try_into().unwrap();
        assert!((f32::from_be_bytes(f4_bytes) - 1.5).abs() < f32::EPSILON);
        // f8: len 8 + BE bits
        assert_eq!(&bytes[10..14], &[0x00, 0x00, 0x00, 0x08]);
        let f8_bytes: [u8; 8] = bytes[14..22].try_into().unwrap();
        assert!((f64::from_be_bytes(f8_bytes) - (-1.25)).abs() < f64::EPSILON);
    }

    #[test]
    fn encode_row_int_widths() {
        let s = schema_of(vec![
            Field::new("a", DataType::Int16, true),
            Field::new("b", DataType::Int32, true),
            Field::new("c", DataType::Int64, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut a = Int16Builder::new();
        a.append_value(-1);
        let mut b = Int32Builder::new();
        b.append_value(0x0102_0304);
        let mut c = Int64Builder::new();
        c.append_value(0x0102_0304_0506_0708);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(a.finish()),
            Arc::new(b.finish()),
            Arc::new(c.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0, 1, 2], 3, 0, &batch, 0);

        // i16 -1 = 0xff 0xff
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x02]);
        assert_eq!(&bytes[6..8], &[0xff, 0xff]);
        // i32 0x01020304 BE
        assert_eq!(&bytes[8..12], &[0x00, 0x00, 0x00, 0x04]);
        assert_eq!(&bytes[12..16], &[0x01, 0x02, 0x03, 0x04]);
        // i64 BE
        assert_eq!(&bytes[16..20], &[0x00, 0x00, 0x00, 0x08]);
        assert_eq!(
            &bytes[20..28],
            &[0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08]
        );
    }

    #[test]
    fn encode_row_date_offset_to_pg_epoch() {
        // 2026-04-25 → days from 1970-01-01 = let's compute
        let target = chrono::NaiveDate::from_ymd_opt(2026, 4, 25).unwrap();
        let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        let unix_days = i32::try_from((target - epoch).num_days()).expect("days fit i32");
        let pg_days = unix_days - PG_EPOCH_DAYS_FROM_UNIX;

        let s = schema_of(vec![
            Field::new("d", DataType::Date32, true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut db = Date32Builder::new();
        db.append_value(unix_days);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(db.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 0, &batch, 0);
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x04]);
        let read = i32::from_be_bytes(bytes[6..10].try_into().unwrap());
        assert_eq!(read, pg_days);
    }

    #[test]
    fn encode_row_timestamptz_offset_to_pg_epoch() {
        // 2026-04-25T00:00:00Z = 1777680000 sec → micros
        let unix_micros: i64 = 1_777_680_000_000_000;
        let pg_micros = unix_micros - PG_EPOCH_MICROS_FROM_UNIX;

        let s = schema_of(vec![
            Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
                true,
            ),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut tb = TimestampMicrosecondBuilder::new().with_timezone("UTC");
        tb.append_value(unix_micros);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(tb.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 0, &batch, 0);
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x08]);
        let read = i64::from_be_bytes(bytes[6..14].try_into().unwrap());
        assert_eq!(read, pg_micros);
    }

    #[test]
    fn encode_row_decimal128_zero() {
        let s = schema_of(vec![
            Field::new("d", DataType::Decimal128(10, 4), true),
            Field::new("geom", DataType::Binary, true),
        ]);
        let mut b = Decimal128Builder::new()
            .with_precision_and_scale(10, 4)
            .unwrap();
        b.append_value(0);
        let cols: Vec<ArrayRef> = vec![
            Arc::new(b.finish()),
            binary_column_with_value(&point_wkb_le()),
        ];
        let batch = RecordBatch::try_new(s.clone(), cols).unwrap();
        let bytes = encode_row_bytes(s, vec![0], 1, 0, &batch, 0);
        // numeric: ndigits=0, weight=0, sign=POS=0x0000, dscale=4 → 8 byte payload, no digit slots.
        assert_eq!(&bytes[2..6], &[0x00, 0x00, 0x00, 0x08]);
        assert_eq!(&bytes[6..14], &[0, 0, 0, 0, 0, 0, 0, 4]);
    }

    #[test]
    fn pgnumeric_zero() {
        let n = PgNumeric::from_i128_scale(0, 4);
        assert_eq!(n.digits, Vec::<i16>::new());
        assert_eq!(n.weight, 0);
        assert_eq!(n.sign, NUMERIC_POS);
        assert_eq!(n.dscale, 4);
    }

    #[test]
    fn pgnumeric_one_scale_zero() {
        // 1
        let n = PgNumeric::from_i128_scale(1, 0);
        assert_eq!(n.digits, vec![1]);
        assert_eq!(n.weight, 0);
        assert_eq!(n.sign, NUMERIC_POS);
        assert_eq!(n.dscale, 0);
    }

    #[test]
    fn pgnumeric_negative_one() {
        let n = PgNumeric::from_i128_scale(-1, 0);
        assert_eq!(n.digits, vec![1]);
        assert_eq!(n.weight, 0);
        assert_eq!(n.sign, NUMERIC_NEG);
    }

    #[test]
    fn pgnumeric_one_point_five() {
        // 1.5 with scale 4 → i128 = 15000
        let n = PgNumeric::from_i128_scale(15_000, 4);
        assert_eq!(n.digits, vec![1, 5000]);
        assert_eq!(n.weight, 0);
        assert_eq!(n.dscale, 4);
        assert_eq!(n.sign, NUMERIC_POS);
    }

    #[test]
    fn pgnumeric_small_fraction() {
        // 0.0001 with scale 4 → i128 = 1
        let n = PgNumeric::from_i128_scale(1, 4);
        assert_eq!(n.digits, vec![1]);
        assert_eq!(n.weight, -1);
        assert_eq!(n.dscale, 4);
    }

    #[test]
    fn pgnumeric_round_trip_i128() {
        // 12345.6789 with scale 4 → i128 = 123456789
        let n = PgNumeric::from_i128_scale(123_456_789, 4);
        assert_eq!(n.digits, vec![1, 2345, 6789]);
        assert_eq!(n.weight, 1);
        assert_eq!(n.to_i128_with_scale(4).unwrap(), 123_456_789);
    }

    #[test]
    fn pgnumeric_round_trip_max_decimal128() {
        // 99999999999999999999999999999999.9999999999  (38, 10)
        // i128 = 10^38 - 1
        let max_38: i128 = 10i128.pow(38) - 1;
        let n = PgNumeric::from_i128_scale(max_38, 10);
        assert_eq!(n.dscale, 10);
        assert_eq!(n.to_i128_with_scale(10).unwrap(), max_38);
    }

    #[test]
    fn pgnumeric_round_trip_negative_min() {
        let v: i128 = -123_456_789;
        let n = PgNumeric::from_i128_scale(v, 4);
        assert_eq!(n.sign, NUMERIC_NEG);
        assert_eq!(n.to_i128_with_scale(4).unwrap(), v);
    }

    #[test]
    fn pgnumeric_round_trip_trailing_zero_value() {
        // 10000 with scale 0 → integer trailing zero
        let n = PgNumeric::from_i128_scale(10_000, 0);
        assert_eq!(n.digits, vec![1]);
        assert_eq!(n.weight, 1);
        assert_eq!(n.to_i128_with_scale(0).unwrap(), 10_000);
    }

    #[test]
    fn pgnumeric_decode_truncates_excess_precision() {
        // wire dscale=4, digits=[1,2345,6789] (=12345.6789), target_scale=2 → 12345.67 → 1234567
        let n = PgNumeric {
            digits: vec![1, 2345, 6789],
            weight: 1,
            sign: NUMERIC_POS,
            dscale: 4,
        };
        assert_eq!(n.to_i128_with_scale(2).unwrap(), 1_234_567);
    }
}
