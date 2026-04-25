//! [`Geom`] の各座標を可変参照で走査するヘルパ。
//!
//! reprojection や座標スケーリングなど、ジオメトリ構造を保ったまま
//! 座標値だけを書き換える処理を `Geom` の variant 展開抜きで書けるようにするための窓口。

use crate::Geom;

/// `g` に含まれる全ての座標を走査し、`f` を呼ぶ。
///
/// 呼び出し順は WKB シリアライズ順（ジオメトリ→リング→点の DFS）。
/// `f` が座標を書き換えると、その変更は `g` に反映される。
///
/// # Examples
///
/// ```
/// use shpx_geom::{geom_walk::for_each_coord_mut, Geom};
///
/// let mut g = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0)]);
/// for_each_coord_mut(&mut g, |x, y| {
///     *x *= 2.0;
///     *y *= 2.0;
/// });
/// assert_eq!(g, Geom::LineString(vec![(0.0, 0.0), (2.0, 2.0)]));
/// ```
pub fn for_each_coord_mut<F>(g: &mut Geom, mut f: F)
where
    F: FnMut(&mut f64, &mut f64),
{
    let _ = try_for_each_coord_mut::<_, ()>(g, |x, y| {
        f(x, y);
        Ok(())
    });
}

/// `for_each_coord_mut` の `Result` 返却版。クロージャがエラーを返した時点で走査を中止する。
/// PROJ 変換のように途中失敗で残点を処理しても無駄な場合に使う。
pub fn try_for_each_coord_mut<F, E>(g: &mut Geom, mut f: F) -> Result<(), E>
where
    F: FnMut(&mut f64, &mut f64) -> Result<(), E>,
{
    walk(g, &mut f)
}

fn walk<F, E>(g: &mut Geom, f: &mut F) -> Result<(), E>
where
    F: FnMut(&mut f64, &mut f64) -> Result<(), E>,
{
    match g {
        Geom::Point(x, y) => f(x, y),
        Geom::LineString(pts) | Geom::MultiPoint(pts) => walk_pts(pts, f),
        Geom::Polygon(rings) | Geom::MultiLineString(rings) => {
            for r in rings {
                walk_pts(r, f)?;
            }
            Ok(())
        }
        Geom::MultiPolygon(polys) => {
            for poly in polys {
                for r in poly {
                    walk_pts(r, f)?;
                }
            }
            Ok(())
        }
    }
}

fn walk_pts<F, E>(pts: &mut [(f64, f64)], f: &mut F) -> Result<(), E>
where
    F: FnMut(&mut f64, &mut f64) -> Result<(), E>,
{
    for (x, y) in pts {
        f(x, y)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_visits_one_coord() {
        let mut g = Geom::Point(1.0, 2.0);
        let mut count = 0;
        for_each_coord_mut(&mut g, |_, _| count += 1);
        assert_eq!(count, 1);
    }

    #[test]
    fn linestring_mutates_in_place() {
        let mut g = Geom::LineString(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)]);
        for_each_coord_mut(&mut g, |x, y| {
            *x += 10.0;
            *y += 20.0;
        });
        assert_eq!(
            g,
            Geom::LineString(vec![(10.0, 20.0), (11.0, 21.0), (12.0, 22.0)])
        );
    }

    #[test]
    fn polygon_with_hole_visits_all_rings() {
        let mut g = Geom::Polygon(vec![
            vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
            vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)],
        ]);
        let mut count = 0;
        for_each_coord_mut(&mut g, |_, _| count += 1);
        assert_eq!(count, 5 + 5);
    }

    #[test]
    fn multipoint_visits_each_point() {
        let mut g = Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0), (2.0, 2.0)]);
        let mut count = 0;
        for_each_coord_mut(&mut g, |_, _| count += 1);
        assert_eq!(count, 3);
    }

    #[test]
    fn multilinestring_visits_all_lines() {
        let mut g = Geom::MultiLineString(vec![
            vec![(0.0, 0.0), (1.0, 1.0)],
            vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0)],
        ]);
        let mut count = 0;
        for_each_coord_mut(&mut g, |_, _| count += 1);
        assert_eq!(count, 2 + 3);
    }

    #[test]
    fn multipolygon_visits_all_rings_in_all_polys() {
        let mut g = Geom::MultiPolygon(vec![
            vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
            vec![
                vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 3.0), (2.0, 2.0)],
                vec![(2.4, 2.4), (2.6, 2.4), (2.6, 2.6), (2.4, 2.6), (2.4, 2.4)],
            ],
        ]);
        let mut count = 0;
        for_each_coord_mut(&mut g, |_, _| count += 1);
        assert_eq!(count, 4 + 5 + 5);
    }

    #[test]
    fn dfs_order_matches_wkb_encoding_order() {
        // MultiPolygon → Polygon → リング → 点 の順で訪問されることを確認する。
        let mut g = Geom::MultiPolygon(vec![
            vec![vec![(1.0, 0.0), (2.0, 0.0)]],
            vec![vec![(3.0, 0.0)], vec![(4.0, 0.0)]],
        ]);
        let mut xs = Vec::new();
        for_each_coord_mut(&mut g, |x, _| xs.push(*x));
        assert_eq!(xs, vec![1.0, 2.0, 3.0, 4.0]);
    }
}
