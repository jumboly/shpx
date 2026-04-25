//! Reader / Writer 双方で使う、1 セル分の所有値表現と geozero `ColumnValue`
//! との相互変換ヘルパ。
//!
//! `flatgeobuf` の `geozero::ColumnValue<'a>` は借用型 (`&str` / `&[u8]`) のため、
//! Arrow 配列から取り出した値を一旦 owned で受けてから borrow し直す必要がある。
//! NULL は `Option<OwnedValue>` の `None` で表現し、`OwnedValue` 自体に Null variant は持たせない。

use geozero::ColumnValue;

/// 1 セルの実体値。FGB ColumnType と概ね 1:1 で対応する variant を持つ。
#[derive(Debug, Clone)]
pub enum OwnedValue {
    Bool(bool),
    Byte(i8),
    UByte(u8),
    Short(i16),
    UShort(u16),
    Int(i32),
    UInt(u32),
    Long(i64),
    ULong(u64),
    Float(f32),
    Double(f64),
    String(String),
    DateTime(String),
    Binary(Vec<u8>),
}

/// `Option<OwnedValue>` を `Option<ColumnValue<'_>>` に借用変換する。
/// `None` は NULL 列として feature の properties から省く合図に使う。
pub fn borrow_column_value(v: &Option<OwnedValue>) -> Option<ColumnValue<'_>> {
    let v = v.as_ref()?;
    Some(match v {
        OwnedValue::Bool(b) => ColumnValue::Bool(*b),
        OwnedValue::Byte(v) => ColumnValue::Byte(*v),
        OwnedValue::UByte(v) => ColumnValue::UByte(*v),
        OwnedValue::Short(v) => ColumnValue::Short(*v),
        OwnedValue::UShort(v) => ColumnValue::UShort(*v),
        OwnedValue::Int(v) => ColumnValue::Int(*v),
        OwnedValue::UInt(v) => ColumnValue::UInt(*v),
        OwnedValue::Long(v) => ColumnValue::Long(*v),
        OwnedValue::ULong(v) => ColumnValue::ULong(*v),
        OwnedValue::Float(v) => ColumnValue::Float(*v),
        OwnedValue::Double(v) => ColumnValue::Double(*v),
        OwnedValue::String(s) => ColumnValue::String(s),
        OwnedValue::DateTime(s) => ColumnValue::DateTime(s),
        OwnedValue::Binary(b) => ColumnValue::Binary(b),
    })
}

/// `geozero::ColumnValue` を `OwnedValue` に実体化する。reader 側で
/// `PropertyProcessor::property` が受け取った値を行単位で保持する用途。
pub fn from_column_value(value: &ColumnValue<'_>) -> OwnedValue {
    match value {
        ColumnValue::Bool(v) => OwnedValue::Bool(*v),
        ColumnValue::Byte(v) => OwnedValue::Byte(*v),
        ColumnValue::UByte(v) => OwnedValue::UByte(*v),
        ColumnValue::Short(v) => OwnedValue::Short(*v),
        ColumnValue::UShort(v) => OwnedValue::UShort(*v),
        ColumnValue::Int(v) => OwnedValue::Int(*v),
        ColumnValue::UInt(v) => OwnedValue::UInt(*v),
        ColumnValue::Long(v) => OwnedValue::Long(*v),
        ColumnValue::ULong(v) => OwnedValue::ULong(*v),
        ColumnValue::Float(v) => OwnedValue::Float(*v),
        ColumnValue::Double(v) => OwnedValue::Double(*v),
        ColumnValue::String(s) | ColumnValue::Json(s) => OwnedValue::String((*s).to_string()),
        ColumnValue::DateTime(s) => OwnedValue::DateTime((*s).to_string()),
        ColumnValue::Binary(b) => OwnedValue::Binary((*b).to_vec()),
    }
}
