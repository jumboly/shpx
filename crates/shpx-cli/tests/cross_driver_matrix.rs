//! CLI レベルの cross-driver matrix。
//!
//! `cli.rs` が個別ペアの roundtrip を確認しているのに対し、本テストは file driver 6 種
//! (SHP / Parquet / GPKG / GeoJSON / FGB / CSV) の cartesian product 30 ペアを
//! 1 ループで検証し、新ペアが追加されても回帰検出が抜けない構造を作る。
//!
//! DB driver (PostGIS / SQL Server / SpatiaLite) は env-gated:
//! - `SHPX_TEST_PG_URL` 有 → PostGIS 経由の cross-driver
//! - `SHPX_TEST_SQLSERVER_URL` 有 → SQL Server 経由
//! - `SHPX_TEST_SPATIALITE=1` 有 → SpatiaLite 経由
//!
//! driver scope の cross-driver 検証 (`crates/shpx-driver-spatialite/tests/cross_driver_roundtrip.rs`)
//! は CLI レイヤを通らない速い回帰検出として残置する。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow_array::{builder::BinaryBuilder, Array, ArrayRef, Int64Array, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use assert_cmd::Command;
use shpx_core::{
    schema::{GeometryMeta, GeometryType, GEOMETRY_META_KEY},
    Crs, Driver, Uri, WriteOpts,
};
use shpx_driver_shp::ShpDriver;
use shpx_geom::wkb::{self, Geom};

/// fixture 用の Point 列（小数 4 桁で WKB に往復しても誤差ゼロな値を選んでいる）。
const FIXTURE_POINTS: &[(f64, f64)] =
    &[(139.7000, 35.7000), (-122.4000, 37.8000), (2.3500, 48.8500)];

/// 1 つの SHP 固定 fixture を生成する。EPSG:4326 / Point / `id` Int64 属性 1 列。
fn make_fixture_shp(path: &Path) {
    let geom_field = {
        let mut f = Field::new("geometry", DataType::Binary, true);
        let meta = GeometryMeta::wkb(GeometryType::Point, Some(Crs::from_epsg(4326)));
        let mut m = HashMap::new();
        m.insert(GEOMETRY_META_KEY.to_string(), meta.to_json().unwrap());
        f.set_metadata(m);
        f
    };
    let id_field = Field::new("id", DataType::Int64, true);
    // SHP/dbf は属性順を保つため、属性 → geometry の順に並べる。
    let schema = Arc::new(Schema::new(vec![id_field, geom_field]));

    let ids: ArrayRef = Arc::new(Int64Array::from(vec![1_i64, 2, 3]));
    let mut bb = BinaryBuilder::new();
    for (x, y) in FIXTURE_POINTS {
        bb.append_value(wkb::encode(&Geom::Point(*x, *y)).unwrap());
    }
    let cols: Vec<ArrayRef> = vec![ids, Arc::new(bb.finish())];
    let batch = RecordBatch::try_new(schema.clone(), cols).unwrap();

    let driver = ShpDriver::new();
    let uri = Uri::from_path(path.to_string_lossy().to_string());
    let mut writer = driver
        .open_write(
            &uri,
            schema,
            Some(Crs::from_epsg(4326)),
            &WriteOpts {
                overwrite: true,
                ..Default::default()
            },
        )
        .unwrap();
    writer.write_batch(&batch).unwrap();
    writer.finish().unwrap();
}

/// final.shp を読み戻して fixture と一致するか確認する。
///
/// CRS は SHP ↔ CSV を経由した経路では `.prj` が再生成されないことがあるが、
/// `--src-crs EPSG:4326` を指定して書き戻すため最終 SHP には .prj が存在する想定。
fn verify_matches_fixture(final_shp: &Path) {
    use shpx_core::ReadOpts;

    let driver = ShpDriver::new();
    let uri = Uri::from_path(final_shp.to_string_lossy().to_string());
    let mut reader = driver.open_read(&uri, &ReadOpts::default()).unwrap();
    let schema = reader.schema();

    // geometry 列を見つける。
    let geom_idx = schema
        .fields()
        .iter()
        .position(|f| f.metadata().contains_key(GEOMETRY_META_KEY))
        .expect("final SHP must have a geometry column");

    let batches: Vec<RecordBatch> = reader.batches().collect::<Result<_, _>>().unwrap();
    let total_rows: usize = batches.iter().map(RecordBatch::num_rows).sum();
    assert_eq!(
        total_rows,
        FIXTURE_POINTS.len(),
        "row count mismatch in {}: got {} expected {}",
        final_shp.display(),
        total_rows,
        FIXTURE_POINTS.len()
    );

    // 全 batch を 1 つに連結して比較する（matrix 規模では batch 分割パターンの差異は本質ではない）。
    let mut idx = 0;
    for batch in &batches {
        let arr = batch
            .column(geom_idx)
            .as_any()
            .downcast_ref::<arrow_array::BinaryArray>()
            .expect("geometry must be Binary");
        for i in 0..batch.num_rows() {
            assert!(!arr.is_null(i), "geometry must be non-null at row {idx}");
            let geom = wkb::decode(arr.value(i)).expect("decode WKB");
            let (x, y) = match geom {
                Geom::Point(x, y) => (x, y),
                other => panic!(
                    "expected Point, got {other:?} at row {idx} in {}",
                    final_shp.display()
                ),
            };
            let (ex, ey) = FIXTURE_POINTS[idx];
            assert!(
                (x - ex).abs() < 1e-9 && (y - ey).abs() < 1e-9,
                "Point mismatch at row {idx} in {}: ({x}, {y}) vs ({ex}, {ey})",
                final_shp.display()
            );
            idx += 1;
        }
    }
}

/// `shpx convert <src> <dst> [extra...]` を呼ぶ。
fn run_convert(src: &Path, dst: &Path, extra: &[&str]) {
    let mut cmd = Command::cargo_bin("shpx").unwrap();
    cmd.args([
        "convert",
        src.to_str().unwrap(),
        dst.to_str().unwrap(),
        "--overwrite",
    ]);
    cmd.args(extra);
    cmd.assert().success();
}

/// CSV など CRS をネイティブで保持しない format から読むときに渡す追加引数。
/// EPSG:4326 を仮定する（fixture と一致）。
fn read_extra_args(src_fmt: &str) -> Vec<&'static str> {
    match src_fmt {
        "csv" => vec!["--src-crs", "EPSG:4326"],
        _ => vec![],
    }
}

fn ext_for(fmt: &str) -> &'static str {
    match fmt {
        "shp" => "shp",
        "parquet" => "parquet",
        "gpkg" => "gpkg",
        "geojson" => "geojson",
        "fgb" => "fgb",
        "csv" => "csv",
        other => panic!("unknown format `{other}`"),
    }
}

#[test]
fn file_driver_mesh_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let fixture_shp = dir.path().join("fixture.shp");
    make_fixture_shp(&fixture_shp);

    let formats = ["shp", "parquet", "gpkg", "geojson", "fgb", "csv"];

    // 各 format に fixture を書き出して staged ファイル群を作る (SHP → fmt の 6 invocation)。
    let mut staged: HashMap<&str, PathBuf> = HashMap::new();
    for fmt in formats {
        let p = dir.path().join(format!("staged.{}", ext_for(fmt)));
        if fmt == "shp" {
            // 既に作った fixture をそのまま使う。SHP writer は EPSG 付き schema で
            // .shp/.dbf/.shx/.prj をすべて出すため 4 つそのままコピーすれば良い。
            for sub in ["shp", "dbf", "shx", "prj"] {
                std::fs::copy(fixture_shp.with_extension(sub), p.with_extension(sub)).unwrap();
            }
        } else {
            run_convert(&fixture_shp, &p, &[]);
        }
        staged.insert(fmt, p);
    }

    // すべてのペア (src_fmt, dst_fmt) で A → B → final.shp を検証する。
    // 同じ format 同士はファイルコピー相当で意味が薄いが、CLI 経路では reader/writer が
    // 入れ替わらない identity ケースとして残しておく価値がある (37 ペアではなく 30 ペアに削る)。
    for &src_fmt in &formats {
        for &dst_fmt in &formats {
            if src_fmt == dst_fmt {
                continue;
            }
            let src_path = &staged[src_fmt];
            let dst_path = dir
                .path()
                .join(format!("conv-{src_fmt}-to-{dst_fmt}.{}", ext_for(dst_fmt)));
            let final_shp = dir.path().join(format!("final-{src_fmt}-{dst_fmt}.shp"));

            run_convert(src_path, &dst_path, &read_extra_args(src_fmt));
            run_convert(&dst_path, &final_shp, &read_extra_args(dst_fmt));
            verify_matches_fixture(&final_shp);
        }
    }
}

// ---------------------------------------------------------------------------
// 以下は env-gated DB driver 経路。手元 / CI の DB が利用可能なときだけ走る。
// ---------------------------------------------------------------------------

fn skip_if_env_missing(name: &str) -> bool {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => false,
        _ => {
            eprintln!("skipping: env `{name}` not set");
            true
        }
    }
}

/// PostGIS ↔ file driver 群の双方向 roundtrip。
#[test]
fn db_postgis_roundtrip_with_files() {
    if skip_if_env_missing("SHPX_TEST_PG_URL") {
        return;
    }
    let pg_base = std::env::var("SHPX_TEST_PG_URL").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fixture_shp = dir.path().join("fixture.shp");
    make_fixture_shp(&fixture_shp);

    // 衝突しないユニークなテーブル名を URI に乗せる (env CI 並走対策)。
    let table = unique_table("shpx_pg_matrix");
    let pg_target = format!("{pg_base}?table={table}");

    // SHP → PostGIS → 各 file format → SHP back の経路を確認する。
    run_convert(&fixture_shp, Path::new(&pg_target), &[]);

    for fmt in ["parquet", "gpkg", "geojson", "fgb", "shp"] {
        let mid = dir.path().join(format!("from-pg.{}", ext_for(fmt)));
        let final_shp = dir.path().join(format!("final-pg-{fmt}.shp"));
        run_convert(Path::new(&pg_target), &mid, &[]);
        run_convert(&mid, &final_shp, &[]);
        verify_matches_fixture(&final_shp);
    }
}

/// SQL Server ↔ file driver 群（geometry 型）。
#[test]
fn db_sqlserver_roundtrip_with_files() {
    if skip_if_env_missing("SHPX_TEST_SQLSERVER_URL") {
        return;
    }
    let mssql_base = std::env::var("SHPX_TEST_SQLSERVER_URL").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let fixture_shp = dir.path().join("fixture.shp");
    make_fixture_shp(&fixture_shp);

    let table = unique_table("shpx_mssql_matrix");
    // SQL Server URI に query を乗せる (geom_type=geometry を明示)。
    let mssql_target = if mssql_base.contains('?') {
        format!("{mssql_base}&table={table}&geom_type=geometry")
    } else {
        format!("{mssql_base}?table={table}&geom_type=geometry")
    };

    run_convert(&fixture_shp, Path::new(&mssql_target), &[]);

    for fmt in ["parquet", "gpkg", "shp"] {
        let mid = dir.path().join(format!("from-mssql.{}", ext_for(fmt)));
        let final_shp = dir.path().join(format!("final-mssql-{fmt}.shp"));
        run_convert(Path::new(&mssql_target), &mid, &[]);
        run_convert(&mid, &final_shp, &[]);
        verify_matches_fixture(&final_shp);
    }
}

/// SpatiaLite ↔ file driver 群。
#[test]
fn db_spatialite_roundtrip_with_files() {
    if skip_if_env_missing("SHPX_TEST_SPATIALITE") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let fixture_shp = dir.path().join("fixture.shp");
    make_fixture_shp(&fixture_shp);

    let sl_path = dir.path().join("matrix.sqlite");
    // file path に `?table=` を載せると `Uri::from_path` の extension parser に
    // `sqlite?table=...` として吸われるため、`sqlite://` URL 形式を使う。
    let sl_uri = format!(
        "sqlite://{}?table=shpx_sl_matrix",
        sl_path.to_string_lossy()
    );

    run_convert(&fixture_shp, Path::new(&sl_uri), &[]);

    for fmt in ["parquet", "gpkg", "shp"] {
        let mid = dir.path().join(format!("from-sl.{}", ext_for(fmt)));
        let final_shp = dir.path().join(format!("final-sl-{fmt}.shp"));
        run_convert(Path::new(&sl_uri), &mid, &[]);
        run_convert(&mid, &final_shp, &[]);
        verify_matches_fixture(&final_shp);
    }
}

/// pid + nanos 由来の重複しにくいテーブル名 (RDB env と並走するときの衝突回避)。
fn unique_table(prefix: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{prefix}_{}_{}", std::process::id(), nanos)
}
