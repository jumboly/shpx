//! staging テーブル経由 bulk insert (案 B、`docs/DESIGN.md` L.219-)。
//!
//! tiberius は geometry / geography UDT の直接 bind を許さず TVP も非対応のため、
//! 一旦 `#shpx_stage_<uuid>` (local temporary table) に WKB + SRID を bulk insert し、
//! `INSERT INTO target SELECT ..., geometry::STGeomFromWKB(...) FROM #stage` で
//! 型変換しながら確定テーブルに転記する。chunk ごとに `BEGIN TRAN` / `COMMIT TRAN`
//! を挟んで tempdb log truncation を可能にする。

use arrow_schema::SchemaRef;
use shpx_core::{Error, Result};
use uuid::Uuid;

use crate::bulk::encode_row;
use crate::conn::SqlClient;
use crate::options::{GeomKind, DEFAULT_BULK_CHUNK, ENV_BULK_CHUNK};
use crate::runtime::runtime;
use crate::type_map::arrow_to_decl;
use crate::util::{driver_err, quote_ident};

/// staging テーブルの WKB 列名。target テーブルの geometry 列名と衝突しないよう、
/// 内部固定の prefix `shpx_` を付ける。
pub(crate) const STAGING_WKB_COL: &str = "shpx_geom_wkb";
/// staging テーブルの SRID 列名。
pub(crate) const STAGING_SRID_COL: &str = "shpx_geom_srid";

/// 接続スコープ local temp テーブル名 `#shpx_stage_<short_uuid>` を生成する。
/// 短縮 uuid (16 文字) で衝突確率 ≪ 1、接続切断で自動 GC。
#[must_use]
pub(crate) fn staging_table_name() -> String {
    let mut hex = Uuid::new_v4().simple().to_string();
    hex.truncate(16);
    format!("#shpx_stage_{hex}")
}

/// target テーブルの属性列と同じ宣言型 + `[shpx_geom_wkb] varbinary(max)` +
/// `[shpx_geom_srid] int` の 3 セクションで staging テーブルを CREATE する SQL。
pub(crate) fn build_staging_create_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    staging_name: &str,
) -> Result<String> {
    let staging = quote_ident(staging_name);
    let mut cols: Vec<String> = Vec::with_capacity(attr_indices.len() + 2);
    for &idx in attr_indices {
        let f = schema.field(idx);
        let decl = arrow_to_decl(f.data_type())?;
        let null_part = if f.is_nullable() { "NULL" } else { "NOT NULL" };
        cols.push(format!("{} {decl} {null_part}", quote_ident(f.name())));
    }
    cols.push(format!(
        "{} varbinary(max) NULL",
        quote_ident(STAGING_WKB_COL)
    ));
    cols.push(format!("{} int NOT NULL", quote_ident(STAGING_SRID_COL)));
    Ok(format!("CREATE TABLE {staging} ({})", cols.join(", ")))
}

/// staging から target への `INSERT INTO ... SELECT ..., {kind}::STGeomFromWKB(...)` 文を組み立てる。
pub(crate) fn build_insert_select_sql(
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    target_qualified: &str,
    staging_name: &str,
    geom_kind: GeomKind,
) -> String {
    let staging = quote_ident(staging_name);
    let attr_quoted: Vec<String> = attr_indices
        .iter()
        .map(|&i| quote_ident(schema.field(i).name()))
        .collect();
    let geom_col = quote_ident(schema.field(geom_index).name());

    let mut target_cols = attr_quoted.clone();
    target_cols.push(geom_col);

    let geom_expr = format!(
        "{}::STGeomFromWKB({}, {})",
        geom_kind.t_sql_name(),
        quote_ident(STAGING_WKB_COL),
        quote_ident(STAGING_SRID_COL)
    );
    let mut select_parts = attr_quoted;
    select_parts.push(geom_expr);

    format!(
        "INSERT INTO {target_qualified} ({}) SELECT {} FROM {staging}",
        target_cols.join(", "),
        select_parts.join(", ")
    )
}

/// chunk size を環境変数 `SHPX_MSSQL_BULK_CHUNK` から取得する。未設定 / parse 失敗時は
/// `DEFAULT_BULK_CHUNK` (100,000)。bench 時のみ env で 1,000,000 に上げる運用。
/// chunk loop 開始時に 1 度だけ呼ばれるので env 読み出しのコストは無視できる。
pub(crate) fn resolve_chunk_size() -> usize {
    std::env::var(ENV_BULK_CHUNK)
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_BULK_CHUNK)
}

/// staging 経由の chunk loop 本体。chunk ごとに `BEGIN TRAN` / bulk_insert /
/// `INSERT…SELECT STGeomFromWKB` / `TRUNCATE` / `COMMIT TRAN` を発行し、最後に
/// staging を `DROP` する (best-effort、接続切断でも消える)。
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_bulk_chunks(
    client: &mut SqlClient,
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    target_qualified: &str,
    geom_kind: GeomKind,
    srid: i32,
    batches: &mut dyn Iterator<Item = Result<arrow_array::RecordBatch>>,
    chunk_size: usize,
) -> Result<()> {
    let staging_name = staging_table_name();
    let create_sql = build_staging_create_sql(schema, attr_indices, &staging_name)?;
    let insert_select_sql = build_insert_select_sql(
        schema,
        attr_indices,
        geom_index,
        target_qualified,
        &staging_name,
        geom_kind,
    );
    let staging_quoted = quote_ident(&staging_name);
    let truncate_sql = format!("TRUNCATE TABLE {staging_quoted}");
    let drop_sql = format!("DROP TABLE {staging_quoted}");

    let rt = runtime()?;

    rt.block_on(async {
        exec_simple(client, create_sql).await?;

        let mut pending: Vec<arrow_array::RecordBatch> = Vec::new();
        let mut pending_rows: usize = 0;

        for res in batches {
            let batch = res?;
            if batch.num_rows() == 0 {
                continue;
            }
            pending_rows += batch.num_rows();
            pending.push(batch);
            if pending_rows >= chunk_size {
                flush_chunk(
                    client,
                    schema,
                    attr_indices,
                    geom_index,
                    &staging_name,
                    &insert_select_sql,
                    &truncate_sql,
                    srid,
                    &pending,
                )
                .await?;
                pending.clear();
                pending_rows = 0;
            }
        }

        if pending_rows > 0 {
            flush_chunk(
                client,
                schema,
                attr_indices,
                geom_index,
                &staging_name,
                &insert_select_sql,
                &truncate_sql,
                srid,
                &pending,
            )
            .await?;
        }

        let _ = exec_simple(client, drop_sql).await;

        Ok::<_, Error>(())
    })
}

#[allow(clippy::too_many_arguments)]
async fn flush_chunk(
    client: &mut SqlClient,
    schema: &SchemaRef,
    attr_indices: &[usize],
    geom_index: usize,
    staging_name: &str,
    insert_select_sql: &str,
    truncate_sql: &str,
    srid: i32,
    batches: &[arrow_array::RecordBatch],
) -> Result<()> {
    let staging_quoted = quote_ident(staging_name);

    exec_simple_str(client, "BEGIN TRAN").await?;

    let mut bulk = client
        .bulk_insert(&staging_quoted)
        .await
        .map_err(|e| driver_err(&e))?;
    for batch in batches {
        for row in 0..batch.num_rows() {
            let token_row = encode_row(schema, batch, attr_indices, geom_index, row, srid)?;
            bulk.send(token_row).await.map_err(|e| driver_err(&e))?;
        }
    }
    bulk.finalize().await.map_err(|e| driver_err(&e))?;

    exec_simple(client, insert_select_sql.to_string()).await?;
    exec_simple(client, truncate_sql.to_string()).await?;
    exec_simple_str(client, "COMMIT TRAN").await?;

    Ok(())
}

async fn exec_simple_str(client: &mut SqlClient, sql: &'static str) -> Result<()> {
    client
        .simple_query(sql)
        .await
        .map_err(|e| driver_err(&e))?
        .into_results()
        .await
        .map_err(|e| driver_err(&e))?;
    Ok(())
}

async fn exec_simple(client: &mut SqlClient, sql: String) -> Result<()> {
    client
        .simple_query(sql)
        .await
        .map_err(|e| driver_err(&e))?
        .into_results()
        .await
        .map_err(|e| driver_err(&e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_schema::{DataType, Field, Schema};
    use std::sync::Arc;

    fn sample_schema() -> SchemaRef {
        Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("geom", DataType::Binary, true),
        ]))
    }

    #[test]
    fn staging_table_name_starts_with_prefix() {
        let n = staging_table_name();
        assert!(n.starts_with("#shpx_stage_"), "got `{n}`");
        // 12 (prefix) + 16 (uuid) = 28 chars
        assert_eq!(n.len(), 28);
    }

    #[test]
    fn build_staging_create_sql_includes_extra_columns() {
        let schema = sample_schema();
        let sql = build_staging_create_sql(&schema, &[0, 1], "#shpx_stage_abc").unwrap();
        assert_eq!(
            sql,
            "CREATE TABLE [#shpx_stage_abc] ([id] int NOT NULL, [name] nvarchar(max) NULL, \
             [shpx_geom_wkb] varbinary(max) NULL, [shpx_geom_srid] int NOT NULL)"
        );
    }

    #[test]
    fn build_insert_select_sql_geometry() {
        let schema = sample_schema();
        let sql = build_insert_select_sql(
            &schema,
            &[0, 1],
            2,
            "[dbo].[t]",
            "#shpx_stage_abc",
            GeomKind::Geometry,
        );
        assert_eq!(
            sql,
            "INSERT INTO [dbo].[t] ([id], [name], [geom]) SELECT [id], [name], \
             geometry::STGeomFromWKB([shpx_geom_wkb], [shpx_geom_srid]) FROM [#shpx_stage_abc]"
        );
    }

    #[test]
    fn build_insert_select_sql_geography() {
        let schema = sample_schema();
        let sql = build_insert_select_sql(
            &schema,
            &[0, 1],
            2,
            "[dbo].[t]",
            "#shpx_stage_abc",
            GeomKind::Geography,
        );
        assert!(sql.contains("geography::STGeomFromWKB"));
    }
}
