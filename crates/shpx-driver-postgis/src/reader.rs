//! PostGIS の `LayerReader` 実装。
//!
//! テーブル全件 SELECT を 1 度だけ block_on で発行し、結果を Arrow `RecordBatch` に
//! 詰めてオブジェクト内で保持する。`batches()` は固定 chunk サイズに分割して列挙する。
//!
//! geometry 列は `ST_AsEWKB(<col>)` で取得し、`shpx_geom::ewkb::strip_srid` で
//! 標準 WKB と SRID に分離する。SRID は `geometry_columns` view → 先頭 non-NULL 行の
//! `ST_SRID()` の順に解決して `Crs` に反映する。

use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{
    builder::{
        ArrayBuilder, BinaryBuilder, BooleanBuilder, Date32Builder, Float32Builder, Float64Builder,
        Int16Builder, Int32Builder, Int64Builder, StringBuilder, TimestampMicrosecondBuilder,
    },
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use chrono::{DateTime, NaiveDate, NaiveDateTime, Utc};
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Error, LayerReader, ReadOpts, Result, Uri,
};
use shpx_geom::ewkb;
use tokio_postgres::{types::Type as PgType, Client, Row};

use crate::conn;
use crate::options::ResolvedReadOpts;
use crate::type_map::{geom_type_from_st_name, pg_to_arrow};
use crate::util::{driver_msg, quote_ident, quote_qualified};

/// 1 batch あたりの既定行数。`batch_size_hint` 未指定時に使う。
const DEFAULT_BATCH_SIZE: usize = 65_536;

pub struct PostgisReader {
    schema: SchemaRef,
    crs: Option<Crs>,
    /// 全行を Arrow に詰めた中間表現。`batches()` で chunk サイズに分割して列挙する。
    rows: Vec<RecordBatch>,
    row_count: usize,
}

impl PostgisReader {
    pub fn open(uri: &Uri, opts: &ReadOpts) -> Result<Self> {
        let resolved = ResolvedReadOpts::resolve(uri, opts)?;
        let qualified = quote_qualified(&resolved.schema, &resolved.table);
        let client = conn::connect(&resolved.url)?;

        let columns = describe_columns(&client, &resolved.schema, &resolved.table)?;
        let geom_col_index = columns.iter().position(|c| c.is_geometry);

        // SRID は geometry 列がある場合のみ問い合わせる。geometry_columns view が未登録
        // なら NULL になりうるので、テーブル先頭行の `ST_SRID() / ST_GeometryType()` も
        // フォールバックに使う。後者は 1 query に集約してラウンドトリップを 1 つ削る。
        let (crs_from_table, geom_type_from_table) = if let Some(geom_idx) = geom_col_index {
            let col_name = &columns[geom_idx].name;
            probe_geometry_metadata(
                &client,
                &resolved.schema,
                &resolved.table,
                col_name,
                &qualified,
            )?
        } else {
            (None, GeometryType::Geometry)
        };

        // ReadOpts.src_crs があれば最優先（CLI --src-crs）。
        let crs: Option<Crs> = opts.src_crs.clone().or(crs_from_table);

        // Arrow Schema を組み立てる。geometry 列は metadata 付きで宣言する。
        let schema =
            build_arrow_schema(&columns, geom_col_index, geom_type_from_table, crs.as_ref())?;

        // SELECT を発行して全行取得。geometry 列は ST_AsEWKB() でラップする。
        let select_sql = build_select_sql(&columns, geom_col_index, &qualified);
        let rows = conn::query(&client, &select_sql, &[])?;
        let batch = rows_to_record_batch(&schema, &columns, geom_col_index, &rows)?;
        let row_count = batch.num_rows();
        let chunks = chunk_batch(&batch, DEFAULT_BATCH_SIZE);

        Ok(Self {
            schema,
            crs,
            rows: chunks,
            row_count,
        })
    }
}

impl LayerReader for PostgisReader {
    fn schema(&self) -> SchemaRef {
        self.schema.clone()
    }

    fn crs(&self) -> Option<&Crs> {
        self.crs.as_ref()
    }

    fn row_count_hint(&self) -> Option<usize> {
        Some(self.row_count)
    }

    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_> {
        Box::new(self.rows.drain(..).map(Ok))
    }
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
    /// PostgreSQL の型名（geometry / geography 判定用）。
    type_name: String,
    /// `tokio_postgres::types::Type` から得た PgType。geometry/geography の場合は本フィールドを参照しない。
    pg_type: PgType,
    /// PostGIS の geometry/geography 列か。
    is_geometry: bool,
    /// NOT NULL 制約。
    nullable: bool,
}

/// `pg_attribute` を引いて列順 + 型を取る。
fn describe_columns(client: &Client, schema: &str, table: &str) -> Result<Vec<ColumnInfo>> {
    // information_schema.columns の `udt_name` だけだと PostGIS geometry の
    // 詳細型 (Point/Polygon 等) は取れないが、geometry/geography 判定には十分。
    // PgType への変換は OID 経由で `Type::from_oid` を使う（tokio-postgres が builtin OID を解決）。
    let sql = "
        SELECT a.attname, a.atttypid, a.attnotnull, t.typname
        FROM pg_attribute a
        JOIN pg_class c ON c.oid = a.attrelid
        JOIN pg_namespace n ON n.oid = c.relnamespace
        JOIN pg_type t ON t.oid = a.atttypid
        WHERE n.nspname = $1
          AND c.relname = $2
          AND a.attnum > 0
          AND NOT a.attisdropped
        ORDER BY a.attnum
    ";
    let rows = conn::query(client, sql, &[&schema, &table])?;
    if rows.is_empty() {
        return Err(driver_msg(format!("table not found: {schema}.{table}")));
    }

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let name: String = row
            .try_get("attname")
            .map_err(|e| driver_msg(e.to_string()))?;
        let oid: u32 = row
            .try_get("atttypid")
            .map_err(|e| driver_msg(e.to_string()))?;
        let notnull: bool = row
            .try_get("attnotnull")
            .map_err(|e| driver_msg(e.to_string()))?;
        let typname: String = row
            .try_get("typname")
            .map_err(|e| driver_msg(e.to_string()))?;

        let is_geom = typname == "geometry" || typname == "geography";
        // builtin 以外の OID（PostGIS geometry など）は Type::from_oid が None を返す。
        // その場合は便宜的に PgType::BYTEA を入れる（geometry 列は本フィールドを使わないため）。
        let pg_type = if is_geom {
            PgType::BYTEA
        } else {
            PgType::from_oid(oid).ok_or_else(|| {
                Error::Schema(format!(
                    "unknown PostgreSQL OID {oid} for column `{name}` (typname `{typname}`)"
                ))
            })?
        };

        out.push(ColumnInfo {
            name,
            type_name: typname,
            pg_type,
            is_geometry: is_geom,
            nullable: !notnull,
        });
    }
    Ok(out)
}

/// geometry 列の SRID と代表 geometry 型を 2 query で取得する:
///
/// 1. `geometry_columns` view（メタ由来、テーブルが空でも srid を返す）
/// 2. 先頭の non-NULL 行から `ST_SRID()` と `ST_GeometryType()` を 1 query で取る
///    （view から srid が取れた場合の fallback と geometry 型の取得を兼ねる）
///
/// view の srid を優先し、見つからなければ probe の srid を使う。
fn probe_geometry_metadata(
    client: &Client,
    schema: &str,
    table: &str,
    geom_col: &str,
    qualified: &str,
) -> Result<(Option<Crs>, GeometryType)> {
    let view_sql = "
        SELECT srid
        FROM geometry_columns
        WHERE f_table_schema = $1 AND f_table_name = $2 AND f_geometry_column = $3
        LIMIT 1
    ";
    let view_srid: Option<i32> =
        match conn::query_opt(client, view_sql, &[&schema, &table, &geom_col])? {
            Some(row) => Some(row.try_get(0).map_err(|e| driver_msg(e.to_string()))?),
            None => None,
        };

    let probe_sql = format!(
        "SELECT ST_SRID({col}), ST_GeometryType({col}) \
         FROM {qualified} WHERE {col} IS NOT NULL LIMIT 1",
        col = quote_ident(geom_col),
    );
    let (probe_srid, geom_type) = match conn::query_opt(client, &probe_sql, &[])? {
        Some(row) => {
            let srid: i32 = row.try_get(0).map_err(|e| driver_msg(e.to_string()))?;
            let name: String = row.try_get(1).map_err(|e| driver_msg(e.to_string()))?;
            (Some(srid), geom_type_from_st_name(&name))
        }
        None => (None, GeometryType::Geometry),
    };

    let crs = view_srid.or(probe_srid).and_then(epsg_to_crs);
    Ok((crs, geom_type))
}

fn epsg_to_crs(srid: i32) -> Option<Crs> {
    if srid <= 0 {
        return None;
    }
    u32::try_from(srid).ok().map(Crs::from_epsg)
}

fn build_arrow_schema(
    columns: &[ColumnInfo],
    geom_col_index: Option<usize>,
    geom_type: GeometryType,
    crs: Option<&Crs>,
) -> Result<SchemaRef> {
    let mut fields: Vec<Field> = Vec::with_capacity(columns.len());
    for (i, c) in columns.iter().enumerate() {
        if Some(i) == geom_col_index {
            let mut f = Field::new(&c.name, DataType::Binary, c.nullable);
            let mut m = HashMap::new();
            m.insert(
                GEOMETRY_META_KEY.to_string(),
                GeometryMeta::wkb(geom_type, crs.cloned()).to_json()?,
            );
            f.set_metadata(m);
            fields.push(f);
        } else {
            let dt = pg_to_arrow(&c.pg_type).map_err(|e| {
                if let Error::Schema(msg) = e {
                    Error::Schema(format!("column `{}` ({}): {}", c.name, c.type_name, msg))
                } else {
                    e
                }
            })?;
            fields.push(Field::new(&c.name, dt, c.nullable));
        }
    }
    Ok(Arc::new(Schema::new(fields)))
}

fn build_select_sql(
    columns: &[ColumnInfo],
    geom_col_index: Option<usize>,
    qualified: &str,
) -> String {
    let parts: Vec<String> = columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            if Some(i) == geom_col_index {
                // geometry を EWKB として取得し、列名は元の名前を保つ。
                format!(
                    "ST_AsEWKB({}) AS {}",
                    quote_ident(&c.name),
                    quote_ident(&c.name)
                )
            } else {
                quote_ident(&c.name)
            }
        })
        .collect();
    format!("SELECT {} FROM {qualified}", parts.join(", "))
}

fn rows_to_record_batch(
    schema: &SchemaRef,
    columns: &[ColumnInfo],
    geom_col_index: Option<usize>,
    rows: &[Row],
) -> Result<RecordBatch> {
    // 各列の builder を schema に基づいて作る。geometry 列は BinaryBuilder。
    let mut builders: Vec<Box<dyn ArrayBuilder>> = schema
        .fields()
        .iter()
        .map(|f| make_builder(f.data_type()))
        .collect();

    for row in rows {
        for (i, c) in columns.iter().enumerate() {
            if Some(i) == geom_col_index {
                let bb = builders[i]
                    .as_any_mut()
                    .downcast_mut::<BinaryBuilder>()
                    .expect("geometry builder is BinaryBuilder");
                let ewkb_bytes: Option<Vec<u8>> =
                    row.try_get(i).map_err(|e| driver_msg(e.to_string()))?;
                match ewkb_bytes {
                    Some(b) => {
                        let (wkb_bytes, _srid) = ewkb::strip_srid(&b)?;
                        bb.append_value(&wkb_bytes);
                    }
                    None => bb.append_null(),
                }
            } else {
                append_value(builders[i].as_mut(), &c.pg_type, row, i, &c.name)?;
            }
        }
    }

    let arrays: Vec<ArrayRef> = builders
        .iter_mut()
        .map(|b| ArrayBuilder::finish(b.as_mut()))
        .collect();
    RecordBatch::try_new(schema.clone(), arrays).map_err(|e| Error::Schema(e.to_string()))
}

fn make_builder(dt: &DataType) -> Box<dyn ArrayBuilder> {
    match dt {
        DataType::Boolean => Box::new(BooleanBuilder::new()),
        DataType::Int16 => Box::new(Int16Builder::new()),
        DataType::Int32 => Box::new(Int32Builder::new()),
        DataType::Int64 => Box::new(Int64Builder::new()),
        DataType::Float32 => Box::new(Float32Builder::new()),
        DataType::Float64 => Box::new(Float64Builder::new()),
        DataType::Utf8 => Box::new(StringBuilder::new()),
        DataType::Binary => Box::new(BinaryBuilder::new()),
        DataType::Date32 => Box::new(Date32Builder::new()),
        DataType::Timestamp(TimeUnit::Microsecond, None) => {
            Box::new(TimestampMicrosecondBuilder::new())
        }
        DataType::Timestamp(TimeUnit::Microsecond, Some(_)) => {
            Box::new(TimestampMicrosecondBuilder::new().with_timezone("UTC"))
        }
        // cycle 1 でサポート外の型は schema 構築段階で reject されるので、ここに来た時点でバグ。
        other => panic!("make_builder: unexpected DataType {other:?}"),
    }
}

#[allow(clippy::too_many_lines)]
fn append_value(
    builder: &mut dyn ArrayBuilder,
    pg_type: &PgType,
    row: &Row,
    col: usize,
    name: &str,
) -> Result<()> {
    let read_err =
        |e: tokio_postgres::Error| -> Error { driver_msg(format!("column `{name}`: {e}")) };
    match *pg_type {
        PgType::BOOL => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BooleanBuilder>()
                .unwrap();
            let v: Option<bool> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT2 => {
            let b = builder.as_any_mut().downcast_mut::<Int16Builder>().unwrap();
            let v: Option<i16> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT4 => {
            let b = builder.as_any_mut().downcast_mut::<Int32Builder>().unwrap();
            let v: Option<i32> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::INT8 => {
            let b = builder.as_any_mut().downcast_mut::<Int64Builder>().unwrap();
            let v: Option<i64> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::FLOAT4 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float32Builder>()
                .unwrap();
            let v: Option<f32> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::FLOAT8 => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Float64Builder>()
                .unwrap();
            let v: Option<f64> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::TEXT | PgType::VARCHAR | PgType::BPCHAR | PgType::NAME => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<StringBuilder>()
                .unwrap();
            let v: Option<String> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(x),
                None => b.append_null(),
            }
        }
        PgType::BYTEA => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<BinaryBuilder>()
                .unwrap();
            let v: Option<Vec<u8>> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(x) => b.append_value(&x),
                None => b.append_null(),
            }
        }
        PgType::DATE => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<Date32Builder>()
                .unwrap();
            let v: Option<NaiveDate> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(d) => {
                    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
                    let days = i32::try_from(d.signed_duration_since(epoch).num_days())
                        .map_err(|_| driver_msg(format!("column `{name}`: date overflow")))?;
                    b.append_value(days);
                }
                None => b.append_null(),
            }
        }
        PgType::TIMESTAMP => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<NaiveDateTime> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(ndt) => {
                    let micros = ndt.and_utc().timestamp_micros();
                    b.append_value(micros);
                }
                None => b.append_null(),
            }
        }
        PgType::TIMESTAMPTZ => {
            let b = builder
                .as_any_mut()
                .downcast_mut::<TimestampMicrosecondBuilder>()
                .unwrap();
            let v: Option<DateTime<Utc>> = row.try_get(col).map_err(read_err)?;
            match v {
                Some(dt) => b.append_value(dt.timestamp_micros()),
                None => b.append_null(),
            }
        }
        ref other => {
            return Err(Error::Schema(format!(
                "column `{name}`: unsupported PostgreSQL type at runtime: {} (OID {})",
                other.name(),
                other.oid()
            )));
        }
    }
    Ok(())
}

/// `RecordBatch` を `chunk_size` 単位の連続スライスに分割する。
fn chunk_batch(batch: &RecordBatch, chunk_size: usize) -> Vec<RecordBatch> {
    if batch.num_rows() <= chunk_size {
        return vec![batch.clone()];
    }
    let mut out = Vec::with_capacity(batch.num_rows().div_ceil(chunk_size));
    let mut offset = 0;
    while offset < batch.num_rows() {
        let len = (batch.num_rows() - offset).min(chunk_size);
        out.push(batch.slice(offset, len));
        offset += len;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epsg_to_crs_handles_zero_and_negative() {
        assert_eq!(epsg_to_crs(0), None);
        assert_eq!(epsg_to_crs(-1), None);
        assert_eq!(epsg_to_crs(4326), Some(Crs::from_epsg(4326)));
    }

    #[test]
    fn build_select_sql_wraps_geometry_with_st_asewkb() {
        let cols = vec![
            ColumnInfo {
                name: "name".into(),
                type_name: "text".into(),
                pg_type: PgType::TEXT,
                is_geometry: false,
                nullable: true,
            },
            ColumnInfo {
                name: "geom".into(),
                type_name: "geometry".into(),
                pg_type: PgType::BYTEA,
                is_geometry: true,
                nullable: true,
            },
        ];
        let sql = build_select_sql(&cols, Some(1), "\"public\".\"t\"");
        assert_eq!(
            sql,
            "SELECT \"name\", ST_AsEWKB(\"geom\") AS \"geom\" FROM \"public\".\"t\""
        );
    }

    #[test]
    fn chunk_batch_splits_uniformly() {
        use arrow_array::Int32Array;
        use arrow_schema::Schema as ASchema;

        let schema = Arc::new(ASchema::new(vec![Field::new("v", DataType::Int32, true)]));
        let arr = Int32Array::from(vec![Some(1), Some(2), Some(3), Some(4), Some(5)]);
        let batch = RecordBatch::try_new(schema, vec![Arc::new(arr)]).unwrap();
        let chunks = chunk_batch(&batch, 2);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].num_rows(), 2);
        assert_eq!(chunks[1].num_rows(), 2);
        assert_eq!(chunks[2].num_rows(), 1);
    }
}
