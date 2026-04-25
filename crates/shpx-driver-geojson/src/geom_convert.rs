//! `geojson::Geometry` ↔ `shpx_geom::Geom` の双方向変換。
//!
//! XY 座標のみ。3D (Z) / 4D (M) 座標と `GeometryCollection` は
//! [`shpx_core::Error::Geometry`] で拒否する（`shpx_geom::Geom` に対応 variant が無いため）。

use geojson::{Geometry as GjGeometry, Value as GjValue};
use shpx_core::{Error, Result};
use shpx_geom::wkb::Geom;

/// `geojson::Geometry` を `shpx_geom::Geom` に変換する。
///
/// `bbox` / `foreign_members` は読み捨てる（ジオメトリ本体には影響しないため）。
pub fn geometry_to_geom(g: &GjGeometry) -> Result<Geom> {
    match &g.value {
        GjValue::Point(pos) => {
            let (x, y) = xy_from_pos(pos)?;
            Ok(Geom::Point(x, y))
        }
        GjValue::LineString(positions) => Ok(Geom::LineString(xy_vec(positions)?)),
        GjValue::Polygon(rings) => Ok(Geom::Polygon(xy_rings(rings)?)),
        GjValue::MultiPoint(positions) => Ok(Geom::MultiPoint(xy_vec(positions)?)),
        GjValue::MultiLineString(lines) => Ok(Geom::MultiLineString(
            lines
                .iter()
                .map(|l| xy_vec(l))
                .collect::<Result<Vec<_>>>()?,
        )),
        GjValue::MultiPolygon(polys) => Ok(Geom::MultiPolygon(
            polys
                .iter()
                .map(|p| xy_rings(p))
                .collect::<Result<Vec<_>>>()?,
        )),
        GjValue::GeometryCollection(_) => Err(Error::Geometry(
            "GeometryCollection is not supported".into(),
        )),
    }
}

/// `shpx_geom::Geom` を `geojson::Geometry` に変換する（XY のみ、必ず成功）。
pub fn geom_to_geometry(g: &Geom) -> GjGeometry {
    let value = match g {
        Geom::Point(x, y) => GjValue::Point(vec![*x, *y]),
        Geom::LineString(pts) => GjValue::LineString(pts.iter().map(xy_to_pos).collect()),
        Geom::Polygon(rings) => GjValue::Polygon(rings_to_pos(rings)),
        Geom::MultiPoint(pts) => GjValue::MultiPoint(pts.iter().map(xy_to_pos).collect()),
        Geom::MultiLineString(lines) => GjValue::MultiLineString(
            lines
                .iter()
                .map(|l| l.iter().map(xy_to_pos).collect())
                .collect(),
        ),
        Geom::MultiPolygon(polys) => {
            GjValue::MultiPolygon(polys.iter().map(|p| rings_to_pos(p)).collect())
        }
    };
    GjGeometry::new(value)
}

fn xy_from_pos(pos: &[f64]) -> Result<(f64, f64)> {
    match pos.len() {
        2 => Ok((pos[0], pos[1])),
        n @ (3 | 4) => Err(Error::Geometry(format!(
            "{n}D coordinates are not supported (Z/M support is planned for a future version)"
        ))),
        other => Err(Error::Geometry(format!(
            "invalid GeoJSON position: expected 2 elements (x, y), got {other}"
        ))),
    }
}

fn xy_vec(positions: &[Vec<f64>]) -> Result<Vec<(f64, f64)>> {
    positions.iter().map(|p| xy_from_pos(p)).collect()
}

fn xy_rings(rings: &[Vec<Vec<f64>>]) -> Result<Vec<Vec<(f64, f64)>>> {
    rings.iter().map(|r| xy_vec(r)).collect()
}

fn xy_to_pos((x, y): &(f64, f64)) -> Vec<f64> {
    vec![*x, *y]
}

fn rings_to_pos(rings: &[Vec<(f64, f64)>]) -> Vec<Vec<Vec<f64>>> {
    rings
        .iter()
        .map(|ring| ring.iter().map(xy_to_pos).collect())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(g: &Geom) {
        let gj = geom_to_geometry(g);
        let back = geometry_to_geom(&gj).unwrap();
        assert_eq!(*g, back);
    }

    #[test]
    fn point_roundtrip() {
        roundtrip(&Geom::Point(1.5, -2.25));
    }

    #[test]
    fn linestring_roundtrip() {
        roundtrip(&Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 0.5)]));
    }

    #[test]
    fn polygon_with_hole_roundtrip() {
        roundtrip(&Geom::Polygon(vec![
            vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
            vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)],
        ]));
    }

    #[test]
    fn multipoint_roundtrip() {
        roundtrip(&Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0)]));
    }

    #[test]
    fn multilinestring_roundtrip() {
        roundtrip(&Geom::MultiLineString(vec![
            vec![(0.0, 0.0), (1.0, 1.0)],
            vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0)],
        ]));
    }

    #[test]
    fn multipolygon_roundtrip() {
        roundtrip(&Geom::MultiPolygon(vec![
            vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
            vec![
                vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 3.0), (2.0, 2.0)],
                vec![(2.4, 2.4), (2.6, 2.4), (2.6, 2.6), (2.4, 2.6), (2.4, 2.4)],
            ],
        ]));
    }

    #[test]
    fn rejects_3d_coordinates() {
        let gj = GjGeometry::new(GjValue::Point(vec![1.0, 2.0, 3.0]));
        let err = geometry_to_geom(&gj).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("3D")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_geometry_collection() {
        let inner = GjGeometry::new(GjValue::Point(vec![0.0, 0.0]));
        let gj = GjGeometry::new(GjValue::GeometryCollection(vec![inner]));
        let err = geometry_to_geom(&gj).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("GeometryCollection")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_position_with_one_element() {
        let gj = GjGeometry::new(GjValue::Point(vec![1.0]));
        let err = geometry_to_geom(&gj).unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }
}
