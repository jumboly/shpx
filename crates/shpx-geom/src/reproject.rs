//! 座標参照系の変換 (reprojection)。
//!
//! `proj` クレートを使って WKB ↔ WKB の変換を行う。入出力 CRS は `Crs` から
//! PROJ user-input 文字列 (`"EPSG:4326"` / WKT2 / proj-string) を導出する。
//! `Proj::new_known_crs` が `proj_normalize_for_visualization` を適用するため、
//! EPSG:4326 のような lat-lon 系も traditional XY (=lon, lat) で扱える。
//!
//! `proj::Proj` は内部に `*mut PJ_AREA` を抱えており `!Send` なので、
//! `Reprojector` は CRS spec 文字列のみ保持し、実体の `Proj` は thread-local に
//! キャッシュする。これにより [`Reprojector`] は `Send + Sync` となり、
//! `LayerWriter: Send` 制約や将来の rayon 並列化と整合する。

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use arrow_array::{builder::BinaryBuilder, Array, ArrayRef, BinaryArray, RecordBatch};
use proj::Proj;
use shpx_core::{Crs, Error, Result};

use crate::wkb;

thread_local! {
    /// (src_spec, dst_spec) → 構築済み `Proj` のスレッド毎キャッシュ。
    /// `Proj::new_known_crs` の構築コストは数 ms 級なので、batch ごとに新規生成すると
    /// オーバーヘッドが大きい。CRS 組合せ単位でメモ化する。
    static PROJ_CACHE: RefCell<HashMap<(String, String), Proj>> = RefCell::new(HashMap::new());
}

/// reproject 用の設定保持型。実 PROJ ハンドルは持たず、スレッド毎に遅延構築・キャッシュする。
pub struct Reprojector {
    src_spec: String,
    dst_spec: String,
}

impl Reprojector {
    /// `src` から `dst` への変換器を構築する。生成時に PROJ パイプラインの妥当性も検証する。
    pub fn new(src: &Crs, dst: &Crs) -> Result<Self> {
        let src_spec = crs_to_proj_spec(src, "source")?;
        let dst_spec = crs_to_proj_spec(dst, "target")?;
        // 早期にエラー検出するため、現在のスレッドでパイプラインを 1 度構築してみる。
        with_proj(&src_spec, &dst_spec, |_| Ok(()))?;
        Ok(Self { src_spec, dst_spec })
    }

    /// 入力 / 出力の CRS spec が一致するか（PROJ user-input 文字列の単純比較）。
    /// 同一なら変換不要として呼び出し側でショートカットできる。
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.src_spec == self.dst_spec
    }

    /// 1 件分の WKB を変換し、新しい WKB バイト列を返す。
    pub fn transform_wkb(&self, bytes: &[u8]) -> Result<Vec<u8>> {
        let mut g = wkb::decode(bytes)?;
        self.transform_geom(&mut g)?;
        wkb::encode(&g)
    }

    /// `Geom` を in-place で変換する。
    pub fn transform_geom(&self, g: &mut crate::Geom) -> Result<()> {
        with_proj(&self.src_spec, &self.dst_spec, |proj| {
            transform_geom_with_proj(proj, g)
        })
    }

    /// `RecordBatch` の `geom_idx` 列の WKB を変換した新しい `RecordBatch` を返す。
    /// その他の列はそのまま流用する。
    pub fn transform_batch(&self, batch: &RecordBatch, geom_idx: usize) -> Result<RecordBatch> {
        let col = batch.column(geom_idx);
        let arr = col.as_any().downcast_ref::<BinaryArray>().ok_or_else(|| {
            Error::Schema(format!(
                "reproject: column {geom_idx} is not BinaryArray (got {:?})",
                col.data_type()
            ))
        })?;

        with_proj(&self.src_spec, &self.dst_spec, |proj| {
            let mut bb = BinaryBuilder::with_capacity(arr.len(), arr.value_data().len());
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    bb.append_null();
                } else {
                    let mut g = wkb::decode(arr.value(i))?;
                    transform_geom_with_proj(proj, &mut g)?;
                    bb.append_value(wkb::encode(&g)?);
                }
            }
            let new_col: ArrayRef = Arc::new(bb.finish());
            let mut cols: Vec<ArrayRef> = batch.columns().to_vec();
            cols[geom_idx] = new_col;
            RecordBatch::try_new(batch.schema(), cols).map_err(|e| Error::Schema(e.to_string()))
        })
    }
}

fn with_proj<F, R>(src_spec: &str, dst_spec: &str, f: F) -> Result<R>
where
    F: FnOnce(&Proj) -> Result<R>,
{
    PROJ_CACHE.with(|cache| {
        let key = (src_spec.to_string(), dst_spec.to_string());
        if !cache.borrow().contains_key(&key) {
            let proj = Proj::new_known_crs(src_spec, dst_spec, None).map_err(|e| {
                Error::Crs(format!(
                    "PROJ pipeline init failed for `{src_spec}` -> `{dst_spec}`: {e}"
                ))
            })?;
            cache.borrow_mut().insert(key.clone(), proj);
        }
        let cache = cache.borrow();
        f(cache.get(&key).expect("just inserted or confirmed present"))
    })
}

fn transform_geom_with_proj(proj: &Proj, g: &mut crate::Geom) -> Result<()> {
    crate::geom_walk::try_for_each_coord_mut(g, |x, y| {
        let (nx, ny) = proj
            .convert((*x, *y))
            .map_err(|e| Error::Geometry(format!("PROJ transform failed at ({x}, {y}): {e}")))?;
        *x = nx;
        *y = ny;
        Ok(())
    })
}

fn crs_to_proj_spec(c: &Crs, role: &str) -> Result<String> {
    if let Some(code) = c.epsg_code() {
        return Ok(format!("EPSG:{code}"));
    }
    if let Some(w) = &c.wkt {
        return Ok(w.clone());
    }
    if let Some(j) = &c.projjson {
        return Ok(j.clone());
    }
    Err(Error::Crs(format!(
        "{role} CRS has neither EPSG authority nor WKT/PROJJSON spec"
    )))
}

/// `--reproject <SPEC>` の値を `Crs` にパースする。
///
/// 受理する形式:
/// - `EPSG:xxxx` （authority に格納）
/// - WKT2 文字列（`PROJCRS[...]` / `GEOGCRS[...]` 等で始まる）
/// - proj-string（`+proj=...` で始まる）
/// - PROJJSON（`{` で始まる）
///
/// EPSG 以外は `Crs.wkt` あるいは `Crs.projjson` フィールドに生のまま格納し、
/// PROJ 入力としてそのまま渡す。書き出し側のメタデータ整合は呼び出し側の責任。
pub fn parse_target_crs(s: &str) -> Result<Crs> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::Crs("--reproject value is empty".into()));
    }
    if let Some(c) = Crs::parse_epsg(s) {
        return Ok(c);
    }
    if s.starts_with('{') {
        return Ok(Crs {
            authority: None,
            wkt: None,
            wkt_flavor: shpx_core::WktFlavor::V2,
            projjson: Some(s.to_string()),
        });
    }
    // proj-string や WKT は `wkt` フィールドに格納して PROJ にパススルーする。
    Ok(Crs {
        authority: None,
        wkt: Some(s.to_string()),
        wkt_flavor: shpx_core::WktFlavor::V2,
        projjson: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Geom;

    #[test]
    fn parse_target_accepts_epsg() {
        let c = parse_target_crs("EPSG:3857").unwrap();
        assert_eq!(c.epsg_code(), Some(3857));
    }

    #[test]
    fn parse_target_accepts_proj_string() {
        let c = parse_target_crs("+proj=longlat +datum=WGS84").unwrap();
        assert!(c.epsg_code().is_none());
        assert!(c.wkt.as_deref().unwrap().starts_with("+proj"));
    }

    #[test]
    fn parse_target_rejects_empty() {
        assert!(parse_target_crs("   ").is_err());
    }

    /// 東京駅付近: EPSG:4326 (139.76710, 35.68124) → EPSG:3857 で
    /// 既知値 (15558325.04, 4256749.97) [m] と 1cm 以下で一致する。
    /// 既知値は PROJ 同等の参考実装で求めたものを丸めた近似で、誤差は cm オーダー。
    #[test]
    fn epsg_4326_to_3857_known_point_within_1cm() {
        let r = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(3857)).unwrap();
        let mut g = Geom::Point(139.767_1, 35.681_24);
        r.transform_geom(&mut g).unwrap();
        let Geom::Point(x, y) = g else {
            panic!("not a point");
        };
        // 期待値は球面メルカトル定義式から: x = 6378137 * λ_rad, y = 6378137 * ln(tan(π/4 + φ_rad/2))
        let lon_rad = 139.767_1_f64.to_radians();
        let lat_rad = 35.681_24_f64.to_radians();
        let r_earth = 6_378_137.0_f64;
        let expected_x = r_earth * lon_rad;
        let expected_y = r_earth * (std::f64::consts::FRAC_PI_4 + lat_rad / 2.0).tan().ln();
        let dx = x - expected_x;
        let dy = y - expected_y;
        assert!(
            dx.hypot(dy) < 0.01,
            "delta = {} m (x={x}, y={y}, exp_x={expected_x}, exp_y={expected_y})",
            dx.hypot(dy)
        );
    }

    /// 4326 → 3857 → 4326 のラウンドトリップが 1cm (≈ 1e-7 度) 以下で一致する。
    #[test]
    fn roundtrip_4326_3857_4326_within_1cm() {
        let fwd = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(3857)).unwrap();
        let bwd = Reprojector::new(&Crs::from_epsg(3857), &Crs::from_epsg(4326)).unwrap();
        let original = Geom::Point(139.767_1, 35.681_24);
        let mut g = original.clone();
        fwd.transform_geom(&mut g).unwrap();
        bwd.transform_geom(&mut g).unwrap();
        let (Geom::Point(ox, oy), Geom::Point(nx, ny)) = (original, g) else {
            panic!("not points");
        };
        // 1cm を緯度経度に換算: 約 1e-7 度。
        let dlon_m = (nx - ox).to_radians() * 6_378_137.0 * oy.to_radians().cos();
        let dlat_m = (ny - oy).to_radians() * 6_378_137.0;
        assert!(
            dlon_m.hypot(dlat_m) < 0.01,
            "roundtrip drift = {} m",
            dlon_m.hypot(dlat_m)
        );
    }

    #[test]
    fn linestring_reprojects_all_points() {
        let r = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(3857)).unwrap();
        let mut g = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (-1.0, -1.0)]);
        r.transform_geom(&mut g).unwrap();
        if let Geom::LineString(pts) = g {
            assert_eq!(pts.len(), 3);
            assert!((pts[0].0).abs() < 1e-6 && (pts[0].1).abs() < 1e-6);
            assert!(pts[1].0 > 0.0 && pts[1].1 > 0.0);
            assert!(pts[2].0 < 0.0 && pts[2].1 < 0.0);
        } else {
            panic!("expected LineString");
        }
    }

    #[test]
    fn polygon_with_hole_reprojects_all_rings() {
        let r = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(3857)).unwrap();
        let mut g = Geom::Polygon(vec![
            vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
            vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)],
        ]);
        r.transform_geom(&mut g).unwrap();
        if let Geom::Polygon(rings) = g {
            assert_eq!(rings.len(), 2);
            assert_eq!(rings[0].len(), 5);
            assert_eq!(rings[1].len(), 5);
        } else {
            panic!("expected Polygon");
        }
    }

    #[test]
    fn missing_crs_spec_errors() {
        let empty = Crs {
            authority: None,
            wkt: None,
            wkt_flavor: shpx_core::WktFlavor::V2,
            projjson: None,
        };
        let r = Reprojector::new(&empty, &Crs::from_epsg(4326));
        assert!(r.is_err());
    }

    #[test]
    fn is_identity_detects_same_spec() {
        let r = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(4326)).unwrap();
        assert!(r.is_identity());
        let r2 = Reprojector::new(&Crs::from_epsg(4326), &Crs::from_epsg(3857)).unwrap();
        assert!(!r2.is_identity());
    }
}
