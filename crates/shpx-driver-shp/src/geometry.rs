//! `shapefile::Shape` ↔ `shpx_geom::wkb::Geom` 相互変換。
//!
//! `shpx_geom::wkb::Geom` は v0.1 で XY のみ対応のため、入力 Shapefile の Z/M shape は
//! `OnLoss` ポリシー適用の上で XY に落とす。出力は常に XY shape (Point/Polyline/Polygon/Multipoint) を返す。

use shapefile::record::traits::HasXY;
use shapefile::{Multipoint, Point, Polygon, PolygonRing, Polyline, Shape, ShapeType};
use shpx_core::{schema::GeometryType, Error, OnLoss, Result};
use shpx_geom::wkb::Geom;

use crate::util::{apply_on_loss, driver_msg, loss_kind};

/// 入力 `ShapeType` から、Arrow 列メタに載せる `GeometryType` を決める。
pub fn shape_type_to_geometry_type(t: ShapeType) -> GeometryType {
    match t {
        ShapeType::Point | ShapeType::PointM | ShapeType::PointZ => GeometryType::Point,
        ShapeType::Multipoint | ShapeType::MultipointM | ShapeType::MultipointZ => {
            GeometryType::MultiPoint
        }
        ShapeType::Polyline | ShapeType::PolylineM | ShapeType::PolylineZ => {
            GeometryType::MultiLineString
        }
        ShapeType::Polygon | ShapeType::PolygonM | ShapeType::PolygonZ => {
            GeometryType::MultiPolygon
        }
        ShapeType::Multipatch | ShapeType::NullShape => GeometryType::Geometry,
    }
}

/// 列メタの `GeometryType` から、出力 `.shp` の `ShapeType` を決める。
///
/// `GeometryType::Geometry` (混在) と `GeometryCollection` は v0.1 ではサポートしない。
pub fn decide_output_shape_type(gt: GeometryType) -> Result<ShapeType> {
    Ok(match gt {
        GeometryType::Point => ShapeType::Point,
        GeometryType::MultiPoint => ShapeType::Multipoint,
        GeometryType::LineString | GeometryType::MultiLineString => ShapeType::Polyline,
        GeometryType::Polygon | GeometryType::MultiPolygon => ShapeType::Polygon,
        GeometryType::Geometry | GeometryType::GeometryCollection => {
            return Err(driver_msg(format!(
                "geometry_type `{gt:?}` is not supported by Shapefile output (no mixed shapes)"
            )));
        }
    })
}

/// `shapefile::Shape` を `Geom` に変換する。
///
/// - `NullShape` → `Ok(None)`（行に geometry 無し）
/// - PointZ/PolygonZ など Z を持つ shape → `OnLoss` 適用後に Z を落として XY を返す
/// - `*M` shape → 同様に M を落として XY を返す
/// - Multipatch → サポート外として `Error::Driver`（v0.1 範囲外）
pub fn shp_to_geom(shape: &Shape, on_loss: OnLoss) -> Result<Option<Geom>> {
    match shape {
        Shape::NullShape => Ok(None),

        // XY 系統。
        Shape::Point(p) => Ok(Some(Geom::Point(p.x, p.y))),
        Shape::Polyline(pl) => Ok(Some(linestring_or_multi(parts_xy(pl.parts())))),
        Shape::Polygon(pg) => Ok(Some(Geom::Polygon(rings_xy(pg.rings())))),
        Shape::Multipoint(mp) => Ok(Some(Geom::MultiPoint(points_xy(mp.points())))),

        // *M 系統。M を落として XY 化。
        Shape::PointM(p) => drop_axis(loss_kind::M_ON_SHP, on_loss, || Geom::Point(p.x, p.y)),
        Shape::PolylineM(pl) => drop_axis(loss_kind::M_ON_SHP, on_loss, || {
            linestring_or_multi(parts_xy(pl.parts()))
        }),
        Shape::PolygonM(pg) => drop_axis(loss_kind::M_ON_SHP, on_loss, || {
            Geom::Polygon(rings_xy(pg.rings()))
        }),
        Shape::MultipointM(mp) => drop_axis(loss_kind::M_ON_SHP, on_loss, || {
            Geom::MultiPoint(points_xy(mp.points()))
        }),

        // *Z 系統 (Z を捨てれば XY 取り出しは同じ extractor で済む)。
        Shape::PointZ(p) => drop_axis(loss_kind::Z_ON_SHP, on_loss, || Geom::Point(p.x, p.y)),
        Shape::PolylineZ(pl) => drop_axis(loss_kind::Z_ON_SHP, on_loss, || {
            linestring_or_multi(parts_xy(pl.parts()))
        }),
        Shape::PolygonZ(pg) => drop_axis(loss_kind::Z_ON_SHP, on_loss, || {
            Geom::Polygon(rings_xy(pg.rings()))
        }),
        Shape::MultipointZ(mp) => drop_axis(loss_kind::Z_ON_SHP, on_loss, || {
            Geom::MultiPoint(points_xy(mp.points()))
        }),

        Shape::Multipatch(_) => Err(driver_msg("Multipatch shape is not supported in v0.1")),
    }
}

fn drop_axis(
    kind: &'static str,
    on_loss: OnLoss,
    build: impl FnOnce() -> Geom,
) -> Result<Option<Geom>> {
    apply_on_loss(kind, "geometry", on_loss)?;
    Ok(Some(build()))
}

fn points_xy<P: HasXY>(pts: &[P]) -> Vec<(f64, f64)> {
    pts.iter().map(|p| (p.x(), p.y())).collect()
}

fn parts_xy<P: HasXY>(parts: &[Vec<P>]) -> Vec<Vec<(f64, f64)>> {
    parts.iter().map(|part| points_xy(part)).collect()
}

fn rings_xy<P: HasXY>(rings: &[shapefile::PolygonRing<P>]) -> Vec<Vec<(f64, f64)>> {
    rings.iter().map(|r| points_xy(r.points())).collect()
}

fn linestring_or_multi(parts: Vec<Vec<(f64, f64)>>) -> Geom {
    // Shapefile の Polyline は単一 part も多 part も含む。
    // GeoArrow / GeoParquet 慣習に揃え、part が 1 本なら LineString、多なら MultiLineString とする。
    if parts.len() == 1 {
        Geom::LineString(parts.into_iter().next().expect("len==1"))
    } else {
        Geom::MultiLineString(parts)
    }
}

/// `Geom` を `shapefile::Shape` (XY のみ) に変換する。
pub fn geom_to_shp(g: &Geom) -> Result<Shape> {
    Ok(match g {
        Geom::Point(x, y) => Shape::Point(Point::new(*x, *y)),
        Geom::LineString(pts) => Shape::Polyline(Polyline::new(to_points(pts))),
        Geom::Polygon(rings) => Shape::Polygon(Polygon::with_rings(rings_to_polygon_rings(rings))),
        Geom::MultiPoint(pts) => Shape::Multipoint(Multipoint::new(to_points(pts))),
        Geom::MultiLineString(parts) => Shape::Polyline(Polyline::with_parts(
            parts.iter().map(|part| to_points(part)).collect(),
        )),
        Geom::MultiPolygon(polys) => {
            // shapefile では複数 Polygon を 1 つの Polygon shape に多 part として詰める。
            let all_rings: Vec<_> = polys
                .iter()
                .flat_map(|r| rings_to_polygon_rings(r))
                .collect();
            Shape::Polygon(Polygon::with_rings(all_rings))
        }
    })
}

fn to_points(pts: &[(f64, f64)]) -> Vec<Point> {
    pts.iter().map(|(x, y)| Point::new(*x, *y)).collect()
}

/// `Geom::Polygon` の rings (先頭が外周、以降は穴) を `shapefile::PolygonRing` の列に変換する。
///
/// 点順序 (CW/CCW) とリング種別の対応は `shapefile` crate 側で正規化される。
fn rings_to_polygon_rings(rings: &[Vec<(f64, f64)>]) -> Vec<PolygonRing<Point>> {
    rings
        .iter()
        .enumerate()
        .map(|(i, ring)| {
            let pts = to_points(ring);
            if i == 0 {
                PolygonRing::Outer(pts)
            } else {
                PolygonRing::Inner(pts)
            }
        })
        .collect()
}

/// `Geom` 値が `ShapeType` に適合しているかを検査する。
///
/// 行ごとのチェックに使い、不整合時は `Error::Geometry` を返す（プランの方針通り `Skip` は採らない）。
pub fn validate_geom_for_shape_type(g: &Geom, st: ShapeType) -> Result<()> {
    let ok = matches!(
        (g, st),
        (Geom::Point(_, _), ShapeType::Point)
            | (Geom::MultiPoint(_), ShapeType::Multipoint)
            | (
                Geom::LineString(_) | Geom::MultiLineString(_),
                ShapeType::Polyline
            )
            | (Geom::Polygon(_) | Geom::MultiPolygon(_), ShapeType::Polygon)
    );
    if !ok {
        return Err(Error::Geometry(format!(
            "row geometry does not match column shape type: row={g:?}, column={st:?}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_xy(g: &Geom) -> Geom {
        let shape = geom_to_shp(g).unwrap();
        shp_to_geom(&shape, OnLoss::Error).unwrap().unwrap()
    }

    #[test]
    fn point_roundtrip() {
        let g = Geom::Point(1.5, -2.25);
        assert_eq!(roundtrip_xy(&g), g);
    }

    #[test]
    fn linestring_roundtrip() {
        let g = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.5)]);
        assert_eq!(roundtrip_xy(&g), g);
    }

    #[test]
    fn polygon_with_hole_roundtrip_preserves_rings() {
        let g = Geom::Polygon(vec![
            vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
            vec![(1.0, 1.0), (1.0, 2.0), (2.0, 2.0), (2.0, 1.0), (1.0, 1.0)],
        ]);
        let back = roundtrip_xy(&g);
        let Geom::Polygon(rings) = back else {
            panic!("expected polygon")
        };
        assert_eq!(rings.len(), 2);
        assert_eq!(rings[0].len(), 5);
        assert_eq!(rings[1].len(), 5);
    }

    #[test]
    fn multipoint_roundtrip() {
        let g = Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0), (2.0, -2.0)]);
        assert_eq!(roundtrip_xy(&g), g);
    }

    #[test]
    fn multilinestring_roundtrip() {
        let g = Geom::MultiLineString(vec![
            vec![(0.0, 0.0), (1.0, 1.0)],
            vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0)],
        ]);
        assert_eq!(roundtrip_xy(&g), g);
    }

    #[test]
    fn pointz_input_under_strict_mode_errors() {
        use shapefile::PointZ;
        let s = Shape::PointZ(PointZ::new(1.0, 2.0, 3.0, 4.0));
        let err = shp_to_geom(&s, OnLoss::Error).unwrap_err();
        match err {
            Error::OnLoss { kind, .. } => assert_eq!(kind, "z-on-shp"),
            other => panic!("expected OnLoss z-on-shp, got {other:?}"),
        }
    }

    #[test]
    fn pointz_input_under_warn_drops_z() {
        use shapefile::PointZ;
        let s = Shape::PointZ(PointZ::new(1.0, 2.0, 3.0, 4.0));
        let g = shp_to_geom(&s, OnLoss::Warn).unwrap().unwrap();
        assert_eq!(g, Geom::Point(1.0, 2.0));
    }

    #[test]
    fn validate_rejects_mismatched_geom() {
        let g = Geom::Point(1.0, 2.0);
        let err = validate_geom_for_shape_type(&g, ShapeType::Polygon).unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }

    #[test]
    fn decide_output_shape_type_rejects_mixed() {
        assert!(decide_output_shape_type(GeometryType::Geometry).is_err());
    }
}
