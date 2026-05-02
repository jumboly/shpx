//! SQL Server の `LayerWriter` / `BulkLoadWriter` 実装。
//!
//! geometry 列は tiberius が UDT 直接 bind 不可のため、`varbinary(max)` (WKB) と
//! `int` (SRID) を 2 引数 bind し、SQL 側で `{geometry|geography}::STGeomFromWKB(@PN, @PS)`
//! に流し込む。案 B (`docs/DESIGN.md` L.219-) の前提で、batch / bulk の両経路で
//! geometry 経路の SQL は同じ shape を使う。

use arrow_array::{
    cast::AsArray,
    types::{
        Date32Type, Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type,
    },
    Array, RecordBatch,
};
use arrow_schema::{DataType, SchemaRef};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use rust_decimal::Decimal;
use shpx_core::{
    schema::{find_geometry_column, GeometryMeta},
    BulkLoadWriter, CreateIndex, CreateTable, Crs, Error, LayerWriter, OnLoss, Result, Uri,
    WriteOpts,
};
use tiberius::ToSql;

use crate::conn::{self, SqlClient};
use crate::options::{GeomKind, ResolvedWriteOpts};
use crate::runtime::runtime;
use crate::staging::{resolve_chunk_size, run_bulk_chunks};
use crate::type_map::arrow_to_decl;
use crate::util::{
    apply_on_loss, bbox_for_epsg, driver_err, driver_msg, loss_kind, primitive, quote_ident,
    quote_qualified, timestamp_to_nanos,
};

pub struct SqlServerWriter {
    client: Option<SqlClient>,
    schema: SchemaRef,
    geom_index: usize,
    attr_indices: Vec<usize>,
    insert_sql: String,
    qualified: String,
    srid: i32,
    /// `finish()` での SPATIAL INDEX 発行時に `geometry` のみ BOUNDING_BOX を要求する分岐に使う。
    geom_kind: GeomKind,
    create_index: CreateIndex,
    /// SPATIAL INDEX 名と対象列を組み立てるため `qualified` とは別に持つ (quote 後の qualified
    /// から逆引きする方が解が無くなるため)。
    geom_col_name: String,
    table_name: String,
}

impl SqlServerWriter {
    pub fn open(
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<&Crs>,
        opts: &WriteOpts,
    ) -> Result<Self> {
        let resolved = ResolvedWriteOpts::resolve(uri, opts)?;
        let qualified = quote_qualified(&resolved.schema, &resolved.table);

        let (geom_index, geom_field_name, geom_meta) = find_geometry_column(&schema)?
            .ok_or_else(|| Error::Schema("no geometry column for SQL Server writer".to_string()))?;

        let attr_indices: Vec<usize> = (0..schema.fields().len())
            .filter(|i| *i != geom_index)
            .collect();

        let mut client = conn::connect(&resolved.url)?;

        let srid = resolve_srid(crs, &geom_meta, resolved.geom_type, opts.on_loss)?;

        if resolved.overwrite {
            conn::simple_query(
                &mut client,
                format!(
                    "IF OBJECT_ID(N'{0}', 'U') IS NOT NULL DROP TABLE {0}",
                    qualified.replace('\'', "''")
                ),
            )?;
        }

        let existed_before = table_exists(&mut client, &resolved.schema, &resolved.table)?;

        // 3 種セマンティクス (PostGIS と同形):
        //   Never:       既存必須。無ければエラー、あれば append のみ
        //   IfNotExists: 既存なら append、無ければ CREATE
        //   Always:      既存なら DROP → CREATE (--overwrite 無しでも DROP するのが Always の契約)
        match resolved.create_table {
            CreateTable::Never => {
                if !existed_before {
                    return Err(driver_msg(format!(
                        "--create-table=never: table {qualified} does not exist"
                    )));
                }
            }
            CreateTable::IfNotExists => {
                if !existed_before {
                    let sql = build_create_table_sql(
                        &schema,
                        &attr_indices,
                        geom_index,
                        &qualified,
                        resolved.geom_type,
                    )?;
                    conn::simple_query(&mut client, sql)?;
                }
            }
            CreateTable::Always => {
                if existed_before {
                    conn::simple_query(&mut client, format!("DROP TABLE {qualified}"))?;
                }
                let sql = build_create_table_sql(
                    &schema,
                    &attr_indices,
                    geom_index,
                    &qualified,
                    resolved.geom_type,
                )?;
                conn::simple_query(&mut client, sql)?;
            }
        }

        let insert_sql = build_insert_sql(
            &schema,
            &attr_indices,
            geom_index,
            &qualified,
            resolved.geom_type,
        );

        Ok(Self {
            client: Some(client),
            schema,
            geom_index,
            attr_indices,
            insert_sql,
            qualified,
            srid,
            geom_kind: resolved.geom_type,
            create_index: resolved.create_index,
            geom_col_name: geom_field_name,
            table_name: resolved.table,
        })
    }

    /// `--create-index` 戦略に従って SPATIAL INDEX を発行する。`finish()` から 1 度だけ呼ぶ。
    ///
    /// SQL Server の `Auto` は no-op に倒す。SPATIAL INDEX は `geometry` 列で
    /// `BOUNDING_BOX` が必須で未知 SRID では失敗するため、暗黙生成は安全側に倒す
    /// (`docs/SQLSERVER.md` の判断記録参照)。`Always` 指定のみ明示有効化する。
    fn maybe_create_spatial_index(&mut self) -> Result<()> {
        if !matches!(self.create_index, CreateIndex::Always) {
            return Ok(());
        }
        let client = self
            .client
            .as_mut()
            .ok_or_else(|| driver_msg("maybe_create_spatial_index called after finish"))?;
        let sql = build_spatial_index_sql(
            &self.qualified,
            &self.table_name,
            &self.geom_col_name,
            self.geom_kind,
            self.srid,
        )?;
        conn::simple_query(client, sql)
    }
}

/// `CREATE SPATIAL INDEX` 文を組み立てる。`geometry` のみ `BOUNDING_BOX` を要求し、
/// 未知 SRID は明示エラー。`geography` は BOUNDING_BOX 不要（経緯度全球が暗黙）。
fn build_spatial_index_sql(
    qualified: &str,
    table_name: &str,
    geom_col_name: &str,
    geom_kind: GeomKind,
    srid: i32,
) -> Result<String> {
    let idx_name = quote_ident(&format!("idx_{table_name}_{geom_col_name}"));
    let geom_col = quote_ident(geom_col_name);
    Ok(match geom_kind {
        GeomKind::Geometry => {
            let srid_u = u32::try_from(srid).unwrap_or(0);
            let bbox = bbox_for_epsg(srid_u).ok_or_else(|| {
                driver_msg(format!(
                    "--create-index=always for `geometry` requires a known SRID for \
                     BOUNDING_BOX (got srid={srid}; supported: 4326, 3857)"
                ))
            })?;
            let (xmin, ymin, xmax, ymax) = bbox;
            format!(
                "CREATE SPATIAL INDEX {idx_name} ON {qualified} ({geom_col}) \
                 WITH (BOUNDING_BOX = ({xmin}, {ymin}, {xmax}, {ymax}))"
            )
        }
        GeomKind::Geography => {
            format!("CREATE SPATIAL INDEX {idx_name} ON {qualified} ({geom_col})")
        }
    })
}

impl LayerWriter for SqlServerWriter {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()> {
        let Self {
            client,
            schema,
            geom_index,
            attr_indices,
            insert_sql,
            srid,
            ..
        } = self;
        let client = client
            .as_mut()
            .ok_or_else(|| driver_msg("write_batch called after finish"))?;
        let rt = runtime()?;

        rt.block_on(async {
            // tiberius に generic な transaction API は無いため BEGIN / COMMIT を文字列で発行する。
            // 1 batch = 1 トランザクションにすることで失敗時の roll-back 単位を batch に揃える。
            client
                .simple_query("BEGIN TRAN")
                .await
                .map_err(|e| driver_err(&e))?
                .into_results()
                .await
                .map_err(|e| driver_err(&e))?;

            for row in 0..batch.num_rows() {
                let owned = build_row_params(schema, batch, attr_indices, *geom_index, row, *srid)?;
                let refs: Vec<&dyn ToSql> = owned.iter().map(|b| &**b as &dyn ToSql).collect();
                let _ = client
                    .execute(insert_sql.as_str(), &refs)
                    .await
                    .map_err(|e| driver_err(&e))?;
            }

            client
                .simple_query("COMMIT TRAN")
                .await
                .map_err(|e| driver_err(&e))?
                .into_results()
                .await
                .map_err(|e| driver_err(&e))?;
            Ok::<_, Error>(())
        })
    }

    fn finish(mut self: Box<Self>) -> Result<()> {
        // bulk / batch 双方で「データ投入完了 → SPATIAL INDEX」の順を統一する (PostGIS の
        // GIST index と同じ位置)。
        self.maybe_create_spatial_index()?;
        let _ = self.client.take();
        Ok(())
    }
}

impl BulkLoadWriter for SqlServerWriter {
    fn bulk_write(
        &mut self,
        batches: &mut dyn Iterator<Item = Result<RecordBatch>>,
    ) -> Result<()> {
        let Self {
            client,
            schema,
            geom_index,
            attr_indices,
            qualified,
            srid,
            geom_kind,
            ..
        } = self;
        let client = client
            .as_mut()
            .ok_or_else(|| driver_msg("bulk_write called after finish"))?;

        run_bulk_chunks(
            client,
            schema,
            attr_indices,
            *geom_index,
            qualified,
            *geom_kind,
            *srid,
            batches,
            resolve_chunk_size(),
        )
    }
}

impl Drop for SqlServerWriter {
    fn drop(&mut self) {
        if self.client.is_some() {
            tracing::warn!(
                target: "shpx::sqlserver",
                table = %self.qualified,
                "SqlServerWriter dropped without finish()"
            );
        }
    }
}

/// CRS と GeomKind から SRID 整数を解決する。geography で CRS 不在の場合は SRID 4326 を
/// 既定とする（geography は有効な geographic CRS を要求するため）。geometry の場合は 0。
fn resolve_srid(
    crs_arg: Option<&Crs>,
    geom_meta: &GeometryMeta,
    geom_kind: GeomKind,
    on_loss: OnLoss,
) -> Result<i32> {
    let crs: Option<Crs> = crs_arg.cloned().or_else(|| geom_meta.crs.clone());
    if let Some(c) = crs.as_ref() {
        if let Some(epsg) = c.epsg_code() {
            return i32::try_from(epsg).map_err(|_| {
                driver_msg(format!("EPSG code out of i32 range: {epsg}"))
            });
        }
    }
    let cont = apply_on_loss(loss_kind::MISSING_CRS_ON_SQLSERVER, "geometry", on_loss)?;
    if !cont {
        // skip ポリシー: SRID 0 で書き続ける（geometry のみ）。geography で skip されると
        // SRID 0 では `geography::STGeomFromWKB` が失敗するため fallback 4326 を返す。
        return Ok(match geom_kind {
            GeomKind::Geometry => 0,
            GeomKind::Geography => 4326,
        });
    }
    Ok(match geom_kind {
        GeomKind::Geometry => 0,
        GeomKind::Geography => 4326,
    })
}

fn build_create_table_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    qualified: &str,
    geom_kind: GeomKind,
) -> Result<String> {
    let mut cols: Vec<String> = Vec::with_capacity(attr_indices.len() + 1);
    for &idx in attr_indices {
        let f = schema.field(idx);
        let decl = arrow_to_decl(f.data_type())?;
        let null_part = if f.is_nullable() { "NULL" } else { "NOT NULL" };
        cols.push(format!("{} {decl} {null_part}", quote_ident(f.name())));
    }
    let geom_field = schema.field(geom_index);
    let geom_decl = geom_kind.t_sql_name();
    let geom_null = if geom_field.is_nullable() {
        "NULL"
    } else {
        "NOT NULL"
    };
    cols.push(format!(
        "{} {geom_decl} {geom_null}",
        quote_ident(geom_field.name())
    ));

    Ok(format!("CREATE TABLE {qualified} ({})", cols.join(", ")))
}

fn build_insert_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    qualified: &str,
    geom_kind: GeomKind,
) -> String {
    let mut col_names: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    col_names.push(quote_ident(schema.field(geom_index).name()));

    // T-SQL parameter は @P1 から始まり、attr 数 + 2 (WKB + SRID) まで使う。
    let attr_count = attr_indices.len();
    let attr_placeholders: Vec<String> =
        (1..=attr_count).map(|i| format!("@P{i}")).collect();
    let wkb_param = format!("@P{}", attr_count + 1);
    let srid_param = format!("@P{}", attr_count + 2);
    let geom_expr = format!(
        "{}::STGeomFromWKB({wkb_param}, {srid_param})",
        geom_kind.t_sql_name()
    );

    let mut value_parts = attr_placeholders;
    value_parts.push(geom_expr);

    format!(
        "INSERT INTO {qualified} ({}) VALUES ({})",
        col_names.join(", "),
        value_parts.join(", ")
    )
}

fn table_exists(client: &mut SqlClient, schema: &str, table: &str) -> Result<bool> {
    let qualified = format!("{}.{}", quote_ident(schema), quote_ident(table));
    let rt = runtime()?;
    let rows = rt.block_on(async {
        let stream = client
            .query(
                "SELECT CASE WHEN OBJECT_ID(@P1, 'U') IS NULL THEN 0 ELSE 1 END",
                &[&qualified],
            )
            .await
            .map_err(|e| driver_err(&e))?;
        stream
            .into_first_result()
            .await
            .map_err(|e| driver_err(&e))
    })?;
    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| driver_msg("table_exists returned no rows"))?;
    let v: Option<i32> = row.try_get(0).map_err(|e| driver_err(&e))?;
    Ok(v.unwrap_or(0) != 0)
}

/// 1 行ぶんのパラメータを所有値で構築する。順序は INSERT の `@P1..@PN` と一致させ、
/// 最後に geometry の WKB と SRID を 2 つ追加する。`ToSql` を実装する型を `Box<dyn ToSql>`
/// に詰めて返し、呼び出し側で `&[&dyn ToSql]` slice に展開する。
type BoxedToSql = Box<dyn ToSql + 'static>;

fn build_row_params(
    schema: &SchemaRef,
    batch: &RecordBatch,
    attr_indices: &[usize],
    geom_index: usize,
    row: usize,
    srid: i32,
) -> Result<Vec<BoxedToSql>> {
    let mut out: Vec<BoxedToSql> = Vec::with_capacity(attr_indices.len() + 2);
    for &col in attr_indices {
        let field = schema.field(col);
        let array: &dyn Array = batch.column(col).as_ref();
        let is_null = array.is_null(row);
        out.push(arrow_to_boxed(field.data_type(), array, row, is_null, field.name())?);
    }
    // geometry: WKB と SRID をその順で詰める（INSERT 文の `@P{n+1}` / `@P{n+2}` に対応）。
    let geom_array = batch.column(geom_index);
    let bb = geom_array.as_binary::<i32>();
    let wkb: Option<Vec<u8>> = if bb.is_null(row) {
        None
    } else {
        Some(bb.value(row).to_vec())
    };
    out.push(Box::new(wkb));
    out.push(Box::new(srid));
    Ok(out)
}

#[allow(clippy::too_many_lines)]
fn arrow_to_boxed(
    dt: &DataType,
    array: &dyn Array,
    row: usize,
    is_null: bool,
    name: &str,
) -> Result<BoxedToSql> {
    Ok(match dt {
        DataType::Boolean => {
            let v: Option<bool> = if is_null { None } else { Some(array.as_boolean().value(row)) };
            Box::new(v)
        }
        DataType::Int16 => {
            let v: Option<i16> = if is_null { None } else { Some(primitive::<Int16Type>(array, row)) };
            Box::new(v)
        }
        DataType::Int32 => {
            let v: Option<i32> = if is_null { None } else { Some(primitive::<Int32Type>(array, row)) };
            Box::new(v)
        }
        DataType::Int64 => {
            let v: Option<i64> = if is_null { None } else { Some(primitive::<Int64Type>(array, row)) };
            Box::new(v)
        }
        DataType::Float32 => {
            let v: Option<f32> = if is_null { None } else { Some(primitive::<Float32Type>(array, row)) };
            Box::new(v)
        }
        DataType::Float64 => {
            let v: Option<f64> = if is_null { None } else { Some(primitive::<Float64Type>(array, row)) };
            Box::new(v)
        }
        DataType::Utf8 => {
            let v: Option<String> = if is_null {
                None
            } else {
                Some(array.as_string::<i32>().value(row).to_string())
            };
            Box::new(v)
        }
        DataType::LargeUtf8 => {
            let v: Option<String> = if is_null {
                None
            } else {
                Some(array.as_string::<i64>().value(row).to_string())
            };
            Box::new(v)
        }
        DataType::Binary => {
            let v: Option<Vec<u8>> = if is_null {
                None
            } else {
                Some(array.as_binary::<i32>().value(row).to_vec())
            };
            Box::new(v)
        }
        DataType::LargeBinary => {
            let v: Option<Vec<u8>> = if is_null {
                None
            } else {
                Some(array.as_binary::<i64>().value(row).to_vec())
            };
            Box::new(v)
        }
        DataType::Date32 => {
            let v: Option<NaiveDate> = if is_null {
                None
            } else {
                let days = primitive::<Date32Type>(array, row);
                let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("epoch");
                Some(
                    epoch
                        .checked_add_signed(Duration::days(i64::from(days)))
                        .ok_or_else(|| driver_msg(format!("column `{name}`: date overflow")))?,
                )
            };
            Box::new(v)
        }
        DataType::Timestamp(unit, None) => {
            let v: Option<NaiveDateTime> = if is_null {
                None
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let secs = nanos.div_euclid(1_000_000_000);
                let nanos_part = nanos.rem_euclid(1_000_000_000);
                let nanos_u = u32::try_from(nanos_part)
                    .map_err(|_| driver_msg(format!("column `{name}`: timestamp nanos overflow")))?;
                let dt = DateTime::<Utc>::from_timestamp(secs, nanos_u)
                    .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp overflow")))?;
                Some(dt.naive_utc())
            };
            Box::new(v)
        }
        DataType::Timestamp(unit, Some(_)) => {
            let v: Option<DateTime<Utc>> = if is_null {
                None
            } else {
                let nanos = timestamp_to_nanos(*unit, array, row, name)?;
                let secs = nanos.div_euclid(1_000_000_000);
                let nanos_part = nanos.rem_euclid(1_000_000_000);
                let nanos_u = u32::try_from(nanos_part)
                    .map_err(|_| driver_msg(format!("column `{name}`: timestamp nanos overflow")))?;
                Some(
                    Utc.timestamp_opt(secs, nanos_u)
                        .single()
                        .ok_or_else(|| driver_msg(format!("column `{name}`: timestamp overflow")))?,
                )
            };
            Box::new(v)
        }
        DataType::Decimal128(_p, s) => {
            let v: Option<Decimal> = if is_null {
                None
            } else {
                let i = primitive::<Decimal128Type>(array, row);
                let scale = u32::try_from(*s).map_err(|_| {
                    driver_msg(format!("column `{name}`: invalid Decimal128 scale {s}"))
                })?;
                Some(Decimal::try_from_i128_with_scale(i, scale).map_err(|e| {
                    driver_msg(format!(
                        "column `{name}`: cannot convert Decimal128 to rust_decimal: {e}"
                    ))
                })?)
            };
            Box::new(v)
        }
        other => {
            return Err(Error::Schema(format!(
                "column `{name}`: unsupported Arrow type for SQL Server bind: {other:?}"
            )));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_schema::{Field, Schema};
    use std::sync::Arc;

    fn sample_schema() -> SchemaRef {
        let mut geom = Field::new("geom", DataType::Binary, true);
        let meta = shpx_core::schema::GeometryMeta::wkb(
            shpx_core::schema::GeometryType::Point,
            None,
        );
        let mut m = std::collections::HashMap::new();
        m.insert(
            shpx_core::schema::GEOMETRY_META_KEY.to_string(),
            meta.to_json().unwrap(),
        );
        geom.set_metadata(m);
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
            geom,
        ]))
    }

    #[test]
    fn build_create_table_sql_geometry() {
        let schema = sample_schema();
        let sql = build_create_table_sql(
            &schema,
            &[0, 1],
            2,
            "[dbo].[t]",
            GeomKind::Geometry,
        )
        .unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE [dbo].[t] ([id] int NOT NULL, [name] nvarchar(max) NULL, [geom] geometry NULL)"
        );
    }

    #[test]
    fn build_create_table_sql_geography() {
        let schema = sample_schema();
        let sql = build_create_table_sql(
            &schema,
            &[0, 1],
            2,
            "[dbo].[t]",
            GeomKind::Geography,
        )
        .unwrap();
        assert!(sql.contains("[geom] geography NULL"));
    }

    #[test]
    fn build_insert_sql_uses_stgeomfromwkb() {
        let schema = sample_schema();
        let sql = build_insert_sql(&schema, &[0, 1], 2, "[dbo].[t]", GeomKind::Geometry);
        assert_eq!(
            sql,
            "INSERT INTO [dbo].[t] ([id], [name], [geom]) \
             VALUES (@P1, @P2, geometry::STGeomFromWKB(@P3, @P4))"
        );
    }

    #[test]
    fn build_insert_sql_geography_changes_udt_only() {
        let schema = sample_schema();
        let sql = build_insert_sql(&schema, &[0, 1], 2, "[dbo].[t]", GeomKind::Geography);
        assert!(sql.contains("geography::STGeomFromWKB(@P3, @P4)"));
    }

    #[test]
    fn resolve_srid_uses_arg_crs() {
        let geom_meta = GeometryMeta::wkb(shpx_core::schema::GeometryType::Point, None);
        let crs = Crs::from_epsg(4326);
        let srid = resolve_srid(Some(&crs), &geom_meta, GeomKind::Geometry, OnLoss::Error).unwrap();
        assert_eq!(srid, 4326);
    }

    #[test]
    fn resolve_srid_falls_back_to_geometry_zero_on_warn() {
        let geom_meta = GeometryMeta::wkb(shpx_core::schema::GeometryType::Point, None);
        let srid = resolve_srid(None, &geom_meta, GeomKind::Geometry, OnLoss::Warn).unwrap();
        assert_eq!(srid, 0);
    }

    #[test]
    fn resolve_srid_falls_back_to_geography_4326_on_warn() {
        let geom_meta = GeometryMeta::wkb(shpx_core::schema::GeometryType::Point, None);
        let srid =
            resolve_srid(None, &geom_meta, GeomKind::Geography, OnLoss::Warn).unwrap();
        assert_eq!(srid, 4326);
    }

    #[test]
    fn resolve_srid_errors_on_missing_crs_with_error_policy() {
        let geom_meta = GeometryMeta::wkb(shpx_core::schema::GeometryType::Point, None);
        let err = resolve_srid(None, &geom_meta, GeomKind::Geometry, OnLoss::Error).unwrap_err();
        assert!(matches!(err, Error::OnLoss { .. }));
    }

    #[test]
    fn spatial_index_sql_geometry_4326() {
        let sql = build_spatial_index_sql("[dbo].[t]", "t", "geom", GeomKind::Geometry, 4326).unwrap();
        assert_eq!(
            sql,
            "CREATE SPATIAL INDEX [idx_t_geom] ON [dbo].[t] ([geom]) \
             WITH (BOUNDING_BOX = (-180, -90, 180, 90))"
        );
    }

    #[test]
    fn spatial_index_sql_geometry_unknown_srid_errors() {
        let err = build_spatial_index_sql("[dbo].[t]", "t", "geom", GeomKind::Geometry, 2451)
            .unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("BOUNDING_BOX"), "msg was: {msg}");
        assert!(msg.contains("2451"), "msg was: {msg}");
    }

    #[test]
    fn spatial_index_sql_geography_no_bounding_box() {
        let sql = build_spatial_index_sql("[dbo].[t]", "t", "geom", GeomKind::Geography, 4326)
            .unwrap();
        assert_eq!(
            sql,
            "CREATE SPATIAL INDEX [idx_t_geom] ON [dbo].[t] ([geom])"
        );
    }
}
