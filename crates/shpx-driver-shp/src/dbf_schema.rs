//! DBF ↔ Arrow の型マッピング、および書き出し計画 (`DbfWritePlan`)。
//!
//! Reader 側: `dbase::FieldInfo` を Arrow `Field` に変換する。
//! Writer 側: 入力 Arrow `Schema` を走査して `DbfWritePlan` を組み立てる。
//! Plan は (Arrow 列 index → DBF フィールド指示) の写像で、表現不能な列は `OnLoss` で
//! スキップ／中断するか、警告ログを出して降格させる。

use std::collections::HashSet;
use std::convert::TryFrom;

use arrow_schema::{DataType, Field, Schema};
use shapefile::dbase::{FieldInfo, FieldName, FieldType, TableWriterBuilder};
use shpx_core::{Error, OnLoss, Result};

use crate::util::{apply_on_loss, driver_msg, loss_kind, truncate_at_char_boundary};

/// 1 つの DBF 出力フィールドの計画。
#[derive(Debug, Clone)]
pub struct DbfFieldPlan {
    /// 入力 `RecordBatch` の何番目の列を読むか。
    pub input_index: usize,
    /// 入力 Arrow フィールドの DataType。書き出し時にキャスト分岐に使う。
    pub data_type: DataType,
    /// 元 Arrow 列名（Arrow 側のメタとして保持）。
    pub source_name: String,
    /// 切詰め・衝突解消後の DBF フィールド名 (10 byte 以内)。
    pub dbf_name: String,
    /// DBF フィールドの種類。
    pub field_type: DbfFieldKind,
}

/// DBF フィールドの種類。長さ・小数桁を構造化して保持する。
#[derive(Debug, Clone)]
pub enum DbfFieldKind {
    /// `C` Character (固定長 byte 列)。
    Character { length: u8 },
    /// `N` Numeric (固定長 ASCII 数値表現)。`decimals=0` で整数。
    Numeric { length: u8, decimals: u8 },
    /// `F` Float (固定長 ASCII 浮動小数)。
    Float { length: u8, decimals: u8 },
    /// `L` Logical (1 byte: T/F)。
    Logical,
    /// `D` Date (YYYYMMDD)。
    Date,
}

/// `plan_dbf_writer_schema` の戻り値。
///
/// `fields` は出力 DBF の物理列順、`skipped` は `OnLoss::Skip`/`Warn` で除外された Arrow 列名。
#[derive(Debug, Clone)]
pub struct DbfWritePlan {
    pub fields: Vec<DbfFieldPlan>,
    pub skipped: Vec<String>,
}

impl DbfWritePlan {
    /// `dbase::TableWriterBuilder` に各フィールドを登録する。
    pub fn apply_to_builder(&self, mut b: TableWriterBuilder) -> Result<TableWriterBuilder> {
        for f in &self.fields {
            let name = FieldName::try_from(f.dbf_name.as_str())
                .map_err(|e| driver_msg(format!("invalid DBF field name `{}`: {e}", f.dbf_name)))?;
            b = match f.field_type {
                DbfFieldKind::Character { length } => b.add_character_field(name, length),
                DbfFieldKind::Numeric { length, decimals } => {
                    b.add_numeric_field(name, length, decimals)
                }
                DbfFieldKind::Float { length, decimals } => {
                    b.add_float_field(name, length, decimals)
                }
                DbfFieldKind::Logical => b.add_logical_field(name),
                DbfFieldKind::Date => b.add_date_field(name),
            };
        }
        Ok(b)
    }
}

// 型マッピング上限（DBF 仕様の伝統的な上限）。
const DBF_NAME_MAX_BYTES: usize = 10;
const DBF_CHARACTER_DEFAULT_LEN: u8 = 254;
const DBF_NUMERIC_MAX_PRECISION: u8 = 18;

/// DBF `FieldInfo` から Arrow `Field` を組み立てる（Reader 用）。
///
/// `dbase::FieldType::Memo` は v0.1 では本文を読まずに長さ可変の文字列として扱う方針のため、
/// Arrow `Utf8` にマップする。
pub fn dbf_field_to_arrow(info: &FieldInfo) -> Field {
    let name = info.name().to_string();
    // `Numeric`/`Float`/`Double`/`Currency` は dbase 0.5 が decimals を非公開のため、
    // 一律 Float64 に揃える（精度保全は v0.2 で `decimals()` 取得 API 追加後に対応）。
    // `Date`/`DateTime` も Arrow 中間表現は Date32 で揃え、時刻は v0.2 まで保持しない。
    let dt = match info.field_type() {
        FieldType::Character | FieldType::Memo => DataType::Utf8,
        FieldType::Numeric | FieldType::Float | FieldType::Double | FieldType::Currency => {
            DataType::Float64
        }
        FieldType::Logical => DataType::Boolean,
        FieldType::Integer => DataType::Int32,
        FieldType::Date | FieldType::DateTime => DataType::Date32,
    };
    Field::new(name, dt, true)
}

/// 入力 Arrow `Schema` から、ジオメトリ列を除いた属性列に対する DBF 書き出し計画を組み立てる。
///
/// `geometry_field_index` は属性列から除外する（DBF に書かない）。
/// `OnLoss` で扱うケース:
/// - 列名 10 byte 超過 → `dbf-name-truncation`
/// - Decimal 精度 18 超過 → `decimal-precision-on-dbf`
/// - Binary/LargeBinary → `binary-on-shp`
/// - Time64/Timestamp(_, Some(tz))/List/Struct/Decimal256 等 → `Error::UnsupportedType` ベース
pub fn plan_dbf_writer_schema(
    schema: &Schema,
    geometry_field_index: Option<usize>,
    on_loss: OnLoss,
) -> Result<DbfWritePlan> {
    let mut fields = Vec::with_capacity(schema.fields().len());
    let mut skipped = Vec::new();
    let mut used_names: HashSet<String> = HashSet::new();

    for (idx, field) in schema.fields().iter().enumerate() {
        if Some(idx) == geometry_field_index {
            continue;
        }
        let source_name = field.name().clone();
        let kind = match map_arrow_to_dbf(field, on_loss)? {
            FieldMapping::Plan(k) => k,
            FieldMapping::Skip => {
                skipped.push(source_name);
                continue;
            }
        };
        let dbf_name = pick_dbf_name(&source_name, &used_names, on_loss)?;
        used_names.insert(dbf_name.clone());
        fields.push(DbfFieldPlan {
            input_index: idx,
            data_type: field.data_type().clone(),
            source_name,
            dbf_name,
            field_type: kind,
        });
    }

    Ok(DbfWritePlan { fields, skipped })
}

enum FieldMapping {
    Plan(DbfFieldKind),
    Skip,
}

fn map_arrow_to_dbf(field: &Field, on_loss: OnLoss) -> Result<FieldMapping> {
    let name = field.name();
    let kind = match field.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 => DbfFieldKind::Character {
            length: DBF_CHARACTER_DEFAULT_LEN,
        },
        DataType::Boolean => DbfFieldKind::Logical,
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16 => {
            DbfFieldKind::Numeric {
                length: 11,
                decimals: 0,
            }
        }
        DataType::Int64 | DataType::UInt32 => DbfFieldKind::Numeric {
            length: 18,
            decimals: 0,
        },
        DataType::Float16 | DataType::Float32 | DataType::Float64 => DbfFieldKind::Float {
            length: 19,
            decimals: 6,
        },
        DataType::Decimal128(p, s) => map_decimal(*p, *s, name, on_loss)?,
        DataType::Date32 | DataType::Date64 => DbfFieldKind::Date,
        DataType::Timestamp(_, tz) => {
            if tz.is_some() && !apply_on_loss(loss_kind::TIMESTAMP_TZ_ON_DBF, name, on_loss)? {
                return Ok(FieldMapping::Skip);
            }
            if !apply_on_loss(loss_kind::TIMESTAMP_TRUNCATE_ON_DBF, name, on_loss)? {
                return Ok(FieldMapping::Skip);
            }
            DbfFieldKind::Date
        }
        DataType::Binary | DataType::LargeBinary | DataType::FixedSizeBinary(_) => {
            if !apply_on_loss(loss_kind::BINARY_ON_SHP, name, on_loss)? {
                return Ok(FieldMapping::Skip);
            }
            // Warn でも DBF に Binary を載せる手段が無い → スキップする (列ごと欠損)。
            return Ok(FieldMapping::Skip);
        }
        // 表現不可能な型は `UnsupportedType` で拒否（OnLoss::Skip では Warn 同等にスキップ）。
        other => {
            return match on_loss {
                OnLoss::Error => Err(Error::UnsupportedType {
                    from: format!("{other:?}"),
                    to: "dbf".to_string(),
                    field: name.clone(),
                }),
                OnLoss::Warn => {
                    tracing::warn!(
                        target: "shpx::shp",
                        kind = "unsupported-type-on-dbf",
                        field = name,
                        from = ?other,
                        "skipping unrepresentable column"
                    );
                    Ok(FieldMapping::Skip)
                }
                OnLoss::Skip => Ok(FieldMapping::Skip),
            };
        }
    };
    Ok(FieldMapping::Plan(kind))
}

fn map_decimal(p: u8, s: i8, field: &str, on_loss: OnLoss) -> Result<DbfFieldKind> {
    let scale = u8::try_from(s.max(0)).unwrap_or(0);
    if p <= DBF_NUMERIC_MAX_PRECISION {
        // `length` は precision + 符号 1 byte ぶんを足しておく（DBF Numeric の慣習）。
        let length = (p + 1).min(20);
        return Ok(DbfFieldKind::Numeric {
            length,
            decimals: scale,
        });
    }
    // 精度超過は `OnLoss` 適用。Warn / Error 共に `apply_on_loss` で分岐される。
    apply_on_loss(loss_kind::DECIMAL_PRECISION_ON_DBF, field, on_loss)?;
    Ok(DbfFieldKind::Numeric {
        length: DBF_NUMERIC_MAX_PRECISION,
        decimals: scale.min(DBF_NUMERIC_MAX_PRECISION.saturating_sub(1)),
    })
}

/// Arrow 列名から、衝突せず 10 byte 以内に収まる DBF フィールド名を選ぶ。
///
/// - 元名が 10 byte 以内かつ未使用 → そのまま採用
/// - 超過していればトランケート + 衝突時に末尾連番 (`name`, `name1`, `name2`, …)
/// - `OnLoss::Error` 下でトランケートが発生したら `Error::OnLoss { kind: "dbf-name-truncation" }`
fn pick_dbf_name(original: &str, used: &HashSet<String>, on_loss: OnLoss) -> Result<String> {
    if original.len() <= DBF_NAME_MAX_BYTES && !used.contains(original) {
        return Ok(original.to_string());
    }
    if original.len() > DBF_NAME_MAX_BYTES {
        apply_on_loss(loss_kind::DBF_NAME_TRUNCATION, original, on_loss)?;
    }
    let base = truncate_at_char_boundary(original, DBF_NAME_MAX_BYTES);
    if !used.contains(base) {
        return Ok(base.to_string());
    }
    // 連番 suffix。`name9999` までで衝突解消できないケースは driver error。
    for n in 1..=9999u32 {
        let suffix = n.to_string();
        let max_base = DBF_NAME_MAX_BYTES.saturating_sub(suffix.len());
        let trimmed = truncate_at_char_boundary(base, max_base);
        let candidate = format!("{trimmed}{suffix}");
        if !used.contains(&candidate) {
            return Ok(candidate);
        }
    }
    Err(driver_msg(format!(
        "could not derive unique DBF name for `{original}`"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_schema::DataType;

    fn arrow_schema(fields: Vec<(&str, DataType)>) -> Schema {
        Schema::new(
            fields
                .into_iter()
                .map(|(n, dt)| Field::new(n, dt, true))
                .collect::<Vec<_>>(),
        )
    }

    #[test]
    fn maps_basic_types() {
        let schema = arrow_schema(vec![
            ("name", DataType::Utf8),
            ("count", DataType::Int32),
            ("flag", DataType::Boolean),
            ("price", DataType::Decimal128(10, 3)),
            ("when", DataType::Date32),
        ]);
        let plan = plan_dbf_writer_schema(&schema, None, OnLoss::Error).unwrap();
        assert_eq!(plan.fields.len(), 5);
        assert!(matches!(
            plan.fields[0].field_type,
            DbfFieldKind::Character { length: 254 }
        ));
        assert!(matches!(
            plan.fields[1].field_type,
            DbfFieldKind::Numeric {
                length: 11,
                decimals: 0
            }
        ));
        assert!(matches!(plan.fields[2].field_type, DbfFieldKind::Logical));
        assert!(matches!(
            plan.fields[3].field_type,
            DbfFieldKind::Numeric {
                length: 11,
                decimals: 3
            }
        ));
        assert!(matches!(plan.fields[4].field_type, DbfFieldKind::Date));
    }

    #[test]
    fn name_truncation_with_collision_under_warn() {
        let schema = arrow_schema(vec![
            ("very_long_field_name_a", DataType::Utf8),
            ("very_long_field_name_b", DataType::Utf8),
        ]);
        let plan = plan_dbf_writer_schema(&schema, None, OnLoss::Warn).unwrap();
        assert_eq!(plan.fields.len(), 2);
        assert_eq!(plan.fields[0].dbf_name, "very_long_");
        // 衝突 → 連番。基底は 9 byte に切られて末尾に "1" が付く。
        assert_eq!(plan.fields[1].dbf_name, "very_long1");
    }

    #[test]
    fn name_truncation_under_strict_errors() {
        let schema = arrow_schema(vec![("very_long_field_name", DataType::Utf8)]);
        let err = plan_dbf_writer_schema(&schema, None, OnLoss::Error).unwrap_err();
        assert!(matches!(err, Error::OnLoss { .. }));
    }

    #[test]
    fn binary_field_with_error_mode_aborts() {
        let schema = arrow_schema(vec![("blob", DataType::Binary)]);
        let err = plan_dbf_writer_schema(&schema, None, OnLoss::Error).unwrap_err();
        match err {
            Error::OnLoss { kind, .. } => assert_eq!(kind, "binary-on-shp"),
            other => panic!("expected OnLoss, got {other:?}"),
        }
    }

    #[test]
    fn binary_field_with_warn_mode_skips_column() {
        let schema = arrow_schema(vec![("blob", DataType::Binary), ("ok", DataType::Int32)]);
        let plan = plan_dbf_writer_schema(&schema, None, OnLoss::Warn).unwrap();
        assert_eq!(plan.fields.len(), 1);
        assert_eq!(plan.fields[0].source_name, "ok");
        assert_eq!(plan.skipped, vec!["blob".to_string()]);
    }

    #[test]
    fn binary_field_with_skip_mode_skips_column() {
        let schema = arrow_schema(vec![("blob", DataType::Binary), ("ok", DataType::Int32)]);
        let plan = plan_dbf_writer_schema(&schema, None, OnLoss::Skip).unwrap();
        assert_eq!(plan.fields.len(), 1);
        assert_eq!(plan.skipped, vec!["blob".to_string()]);
    }

    #[test]
    fn decimal_overflow_truncates_under_warn() {
        let schema = arrow_schema(vec![("amt", DataType::Decimal128(20, 4))]);
        let plan = plan_dbf_writer_schema(&schema, None, OnLoss::Warn).unwrap();
        assert!(matches!(
            plan.fields[0].field_type,
            DbfFieldKind::Numeric {
                length: 18,
                decimals: 4
            }
        ));
    }

    #[test]
    fn decimal_overflow_under_strict_errors() {
        let schema = arrow_schema(vec![("amt", DataType::Decimal128(20, 4))]);
        let err = plan_dbf_writer_schema(&schema, None, OnLoss::Error).unwrap_err();
        match err {
            Error::OnLoss { kind, .. } => assert_eq!(kind, "decimal-precision-on-dbf"),
            other => panic!("expected OnLoss, got {other:?}"),
        }
    }

    #[test]
    fn geometry_column_index_is_excluded() {
        let schema = arrow_schema(vec![
            ("a", DataType::Int32),
            ("geom", DataType::Binary),
            ("b", DataType::Int32),
        ]);
        // index 1 がジオメトリ列（Binary だが geometry 専用扱いで OnLoss を発火させない）。
        let plan = plan_dbf_writer_schema(&schema, Some(1), OnLoss::Error).unwrap();
        assert_eq!(plan.fields.len(), 2);
        assert_eq!(plan.fields[0].source_name, "a");
        assert_eq!(plan.fields[1].source_name, "b");
    }
}
