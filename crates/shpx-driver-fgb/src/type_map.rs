//! Arrow ↔ FlatGeobuf ColumnType / GeometryType のマッピング。
//!
//! FGB の `ColumnType` (`flatgeobuf::ColumnType`) 列挙は FlatBuffers の生成型で、
//! Bool / Byte / UByte / Short / UShort / Int / UInt / Long / ULong / Float / Double /
//! String / Json / DateTime / Binary を取る。Date / Timestamp は Arrow 側で型を保持するが
//! FGB 側では `DateTime`（ISO8601 文字列）に正規化して書き出す。

use arrow_schema::{DataType, Field, TimeUnit};
use flatgeobuf::ColumnType;
use shpx_core::{
    schema::{GeometryMeta, GeometryType},
    Error, Result,
};

use crate::util::DRIVER_NAME;

/// 1 列分の FGB column 定義。`flatgeobuf::ColumnBuilder` に流し込むための情報を保持する。
#[derive(Debug, Clone)]
pub struct FgbColumnPlan {
    pub name: String,
    pub column_type: ColumnType,
    pub nullable: bool,
    /// Decimal 由来など、precision を残しておきたい場合に使う（FGB ヘッダの `precision` フィールド）。
    pub precision: i32,
    /// 同上 `scale`。
    pub scale: i32,
    /// 同上 `width`。Decimal の precision を退避する用途で使う。
    pub width: i32,
}

impl FgbColumnPlan {
    fn new(name: &str, column_type: ColumnType, nullable: bool) -> Self {
        Self {
            name: name.to_string(),
            column_type,
            nullable,
            precision: -1,
            scale: -1,
            width: -1,
        }
    }
}

/// Arrow `Field` を FGB の column 計画に変換する。
///
/// サポート外の型は [`Error::UnsupportedType`] を返す。Decimal は呼び出し側が
/// `apply_on_loss` を経由してから本関数に再投入することを前提とし、ここでは
/// Double + width=precision 情報の保存に変換する。
pub fn arrow_field_to_fgb_column(field: &Field) -> Result<FgbColumnPlan> {
    let nullable = field.is_nullable();
    let name = field.name();
    let plan = match field.data_type() {
        DataType::Boolean => FgbColumnPlan::new(name, ColumnType::Bool, nullable),
        DataType::Int8 => FgbColumnPlan::new(name, ColumnType::Byte, nullable),
        DataType::Int16 => FgbColumnPlan::new(name, ColumnType::Short, nullable),
        DataType::Int32 => FgbColumnPlan::new(name, ColumnType::Int, nullable),
        DataType::Int64 => FgbColumnPlan::new(name, ColumnType::Long, nullable),
        DataType::UInt8 => FgbColumnPlan::new(name, ColumnType::UByte, nullable),
        DataType::UInt16 => FgbColumnPlan::new(name, ColumnType::UShort, nullable),
        DataType::UInt32 => FgbColumnPlan::new(name, ColumnType::UInt, nullable),
        DataType::UInt64 => FgbColumnPlan::new(name, ColumnType::ULong, nullable),
        DataType::Float32 => FgbColumnPlan::new(name, ColumnType::Float, nullable),
        DataType::Float64 => FgbColumnPlan::new(name, ColumnType::Double, nullable),
        DataType::Utf8 | DataType::LargeUtf8 => {
            FgbColumnPlan::new(name, ColumnType::String, nullable)
        }
        DataType::Binary | DataType::LargeBinary => {
            FgbColumnPlan::new(name, ColumnType::Binary, nullable)
        }
        // Date / Timestamp は ISO8601 文字列で `DateTime` カラムに格納する。
        DataType::Date32 | DataType::Date64 | DataType::Timestamp(_, _) => {
            FgbColumnPlan::new(name, ColumnType::DateTime, nullable)
        }
        // Decimal は Double + precision/scale 注記。値の文字列化は writer 側で fallback。
        DataType::Decimal128(p, s) | DataType::Decimal256(p, s) => {
            let mut plan = FgbColumnPlan::new(name, ColumnType::Double, nullable);
            plan.precision = i32::from(*p);
            plan.scale = i32::from(*s);
            plan.width = i32::from(*p);
            plan
        }
        other => {
            return Err(Error::UnsupportedType {
                from: format!("{other:?}"),
                to: DRIVER_NAME.to_string(),
                field: name.clone(),
            });
        }
    };
    Ok(plan)
}

/// FGB column を Arrow `Field` に逆変換する。
///
/// `DateTime` は `Timestamp(Microsecond, Some("UTC"))` に復元する（writer が UTC 文字列で
/// 出力する規約）。`Date32` への絞り込みは reader 側で「時刻部が無い」ことを根拠に
/// 個別判定するため、本関数はその絞り込みを行わない。
//
// `ColumnType` は FlatBuffers 生成型で `ColumnType(_)` の網羅変種を含むため、
// 既知変種と未知変種の双方が `Utf8` に倒れる。`match_same_arms` lint は仕様上の対称性
// を捨てて wildcard 1 本にまとめるよう促すが、文字列に倒す経路の意味づけ
// （String/Json は既知の文字列、`_` は未知 fallback）を残したいため allow する。
#[allow(clippy::match_same_arms)]
pub fn fgb_column_to_arrow_field(name: &str, column_type: ColumnType, nullable: bool) -> Field {
    let dt = match column_type {
        ColumnType::Bool => DataType::Boolean,
        ColumnType::Byte => DataType::Int8,
        ColumnType::UByte => DataType::UInt8,
        ColumnType::Short => DataType::Int16,
        ColumnType::UShort => DataType::UInt16,
        ColumnType::Int => DataType::Int32,
        ColumnType::UInt => DataType::UInt32,
        ColumnType::Long => DataType::Int64,
        ColumnType::ULong => DataType::UInt64,
        ColumnType::Float => DataType::Float32,
        ColumnType::Double => DataType::Float64,
        ColumnType::String | ColumnType::Json => DataType::Utf8,
        ColumnType::DateTime => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        ColumnType::Binary => DataType::Binary,
        // 不明値は安全側で文字列扱い（FGB の将来型用フォールバック）。
        _ => DataType::Utf8,
    };
    Field::new(name, dt, nullable)
}

/// shpx の `GeometryType` を FGB の `GeometryType` (FlatBuffers 生成型) に変換する。
pub fn shpx_to_fgb_geometry_type(meta: &GeometryMeta) -> flatgeobuf::GeometryType {
    use flatgeobuf::GeometryType as G;
    match meta.geometry_type {
        GeometryType::Geometry => G::Unknown,
        GeometryType::Point => G::Point,
        GeometryType::LineString => G::LineString,
        GeometryType::Polygon => G::Polygon,
        GeometryType::MultiPoint => G::MultiPoint,
        GeometryType::MultiLineString => G::MultiLineString,
        GeometryType::MultiPolygon => G::MultiPolygon,
        GeometryType::GeometryCollection => G::GeometryCollection,
    }
}

/// 逆方向: FGB `GeometryType` → shpx `GeometryType`。
pub fn fgb_to_shpx_geometry_type(g: flatgeobuf::GeometryType) -> GeometryType {
    use flatgeobuf::GeometryType as G;
    match g {
        G::Point => GeometryType::Point,
        G::LineString => GeometryType::LineString,
        G::Polygon => GeometryType::Polygon,
        G::MultiPoint => GeometryType::MultiPoint,
        G::MultiLineString => GeometryType::MultiLineString,
        G::MultiPolygon => GeometryType::MultiPolygon,
        G::GeometryCollection => GeometryType::GeometryCollection,
        _ => GeometryType::Geometry,
    }
}
