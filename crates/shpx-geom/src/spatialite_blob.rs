//! SpatiaLite 4.x の geometry blob format (encode / decode)。
//!
//! SpatiaLite の geometry 列は SQLite の BLOB に独自バイナリで格納される。
//! 標準 WKB とは異なるフレーミング（START/MBR/END マーカー、SRID 埋め込み、
//! Multi 系の各子要素直前に `GAIA_MARK_ENTITY` (0x69)）を持つため、
//! `shpx-geom::wkb` とは独立したコーデックとして実装する。
//!
//! # バイト構造（v0.5 で対応する XY のみ）
//!
//! ```text
//! 1 byte  : 0x00                 GAIA_BLOB_START
//! 1 byte  : 0x00 (BE) / 0x01 (LE)
//! 4 bytes : SRID                 i32 in chosen endian
//! 8 bytes : MBR min_x            f64 in chosen endian
//! 8 bytes : MBR min_y
//! 8 bytes : MBR max_x
//! 8 bytes : MBR max_y
//! 1 byte  : 0x7C                 GAIA_MBR_END
//! 4 bytes : geometry class       i32 in chosen endian
//!                                (1=Point, 2=LineString, 3=Polygon,
//!                                 4=MultiPoint, 5=MultiLineString,
//!                                 6=MultiPolygon, 7=GeometryCollection)
//! ...     : 型別 payload。Multi 系の各子要素先頭に 0x69 (GAIA_MARK_ENTITY) が入る点が
//!           標準 WKB との相違（標準 WKB はサブごとに byte order prefix を持つが、
//!           SpatiaLite blob は header に 1 度だけ endian があり、サブには 0x69 が入る）。
//! 1 byte  : 0xFE                 GAIA_BLOB_END
//! ```
//!
//! v0.5 のスコープ:
//! - XY のみ。Z/M / EMPTY / GeometryCollection は [`Error::Geometry`] で拒否する。
//! - encoder は LE 固定で書き、decoder は LE/BE 双方を読める。
//! - MBR は WKB を [`crate::geom_walk::for_each_coord`] で走査して算出する。

use shpx_core::{Error, Result};

use crate::{geom_walk::for_each_coord, wkb, Geom};

const GAIA_BLOB_START: u8 = 0x00;
const GAIA_MBR_END: u8 = 0x7C;
const GAIA_BLOB_END: u8 = 0xFE;
const GAIA_MARK_ENTITY: u8 = 0x69;

const ENDIAN_BE: u8 = 0x00;
const ENDIAN_LE: u8 = 0x01;

const TY_POINT: u32 = 1;
const TY_LINESTRING: u32 = 2;
const TY_POLYGON: u32 = 3;
const TY_MULTIPOINT: u32 = 4;
const TY_MULTILINESTRING: u32 = 5;
const TY_MULTIPOLYGON: u32 = 6;
const TY_GEOMETRYCOLLECTION: u32 = 7;

/// 標準 WKB を SpatiaLite geometry blob にエンコードする (LE 固定)。
///
/// `wkb` は [`crate::wkb::encode`] が生成するバイト列を想定する。
/// 受け取った WKB を一度 [`Geom`] に decode してから SpatiaLite 表現で書き直す。
/// MBR は同じ走査で算出するため、空 geometry は表現できない（v0.5 範囲外）。
pub fn encode(srid: i32, wkb: &[u8]) -> Result<Vec<u8>> {
    let geom = wkb::decode(wkb)?;
    let (min_x, min_y, max_x, max_y) = compute_mbr(&geom)?;

    let mut buf = Vec::with_capacity(estimate_size(&geom));

    buf.push(GAIA_BLOB_START);
    buf.push(ENDIAN_LE);
    buf.extend_from_slice(&srid.to_le_bytes());
    buf.extend_from_slice(&min_x.to_le_bytes());
    buf.extend_from_slice(&min_y.to_le_bytes());
    buf.extend_from_slice(&max_x.to_le_bytes());
    buf.extend_from_slice(&max_y.to_le_bytes());
    buf.push(GAIA_MBR_END);

    write_inner(&mut buf, &geom)?;

    buf.push(GAIA_BLOB_END);
    Ok(buf)
}

/// SpatiaLite geometry blob を `(srid, 標準 WKB)` にデコードする。
///
/// 内部 geometry を [`Geom`] に組み立て直したうえで [`crate::wkb::encode`] で
/// 標準 WKB (LE) を再構築する。MBR は読み飛ばす（再エンコードで信頼可能な
/// 値が再算出されるため、blob 中の MBR をそのまま信用しない）。
pub fn decode(blob: &[u8]) -> Result<(i32, Vec<u8>)> {
    if blob.len() < 39 + 4 + 1 {
        // header(39) + type(4) + end(1) が最小。
        return Err(Error::Geometry(format!(
            "spatialite blob too short: {} bytes",
            blob.len()
        )));
    }
    if blob[0] != GAIA_BLOB_START {
        return Err(Error::Geometry(format!(
            "spatialite blob: bad start marker {:#x}",
            blob[0]
        )));
    }
    let little = match blob[1] {
        ENDIAN_LE => true,
        ENDIAN_BE => false,
        other => {
            return Err(Error::Geometry(format!(
                "spatialite blob: bad endian byte {other:#x}"
            )));
        }
    };
    let srid = read_i32(&blob[2..6], little);
    // MBR (32 bytes) は読み飛ばす。
    if blob[38] != GAIA_MBR_END {
        return Err(Error::Geometry(format!(
            "spatialite blob: bad MBR end marker {:#x}",
            blob[38]
        )));
    }

    let mut c = Cursor::new(blob, 39);
    let geom = read_inner(&mut c, little)?;
    let end = c.take(1)?[0];
    if end != GAIA_BLOB_END {
        return Err(Error::Geometry(format!(
            "spatialite blob: bad end marker {end:#x}"
        )));
    }
    if c.pos != blob.len() {
        return Err(Error::Geometry(format!(
            "spatialite blob: {} trailing bytes",
            blob.len() - c.pos
        )));
    }

    let wkb_bytes = wkb::encode(&geom)?;
    Ok((srid, wkb_bytes))
}

fn compute_mbr(g: &Geom) -> Result<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for_each_coord(g, |x, y| {
        if x < min_x {
            min_x = x;
        }
        if y < min_y {
            min_y = y;
        }
        if x > max_x {
            max_x = x;
        }
        if y > max_y {
            max_y = y;
        }
    });
    if !min_x.is_finite() || !min_y.is_finite() || !max_x.is_finite() || !max_y.is_finite() {
        return Err(Error::Geometry(
            "empty geometry not supported in SpatiaLite blob (v0.5)".into(),
        ));
    }
    Ok((min_x, min_y, max_x, max_y))
}

fn estimate_size(g: &Geom) -> usize {
    // header(39) + type(4) + payload + end(1)
    let payload = match g {
        Geom::Point(_, _) => 16,
        Geom::LineString(pts) => 4 + pts.len() * 16,
        Geom::Polygon(rings) => 4 + rings.iter().map(|r| 4 + r.len() * 16).sum::<usize>(),
        Geom::MultiPoint(pts) => 4 + pts.len() * (1 + 4 + 16),
        Geom::MultiLineString(lines) => {
            4 + lines
                .iter()
                .map(|l| 1 + 4 + 4 + l.len() * 16)
                .sum::<usize>()
        }
        Geom::MultiPolygon(polys) => {
            4 + polys
                .iter()
                .map(|p| 1 + 4 + 4 + p.iter().map(|r| 4 + r.len() * 16).sum::<usize>())
                .sum::<usize>()
        }
    };
    39 + 4 + payload + 1
}

fn write_inner(buf: &mut Vec<u8>, g: &Geom) -> Result<()> {
    match g {
        Geom::Point(x, y) => {
            write_u32_le(buf, TY_POINT);
            write_xy(buf, *x, *y);
        }
        Geom::LineString(pts) => {
            write_u32_le(buf, TY_LINESTRING);
            write_count_le(buf, pts.len())?;
            for &(x, y) in pts {
                write_xy(buf, x, y);
            }
        }
        Geom::Polygon(rings) => {
            write_u32_le(buf, TY_POLYGON);
            write_count_le(buf, rings.len())?;
            for ring in rings {
                write_count_le(buf, ring.len())?;
                for &(x, y) in ring {
                    write_xy(buf, x, y);
                }
            }
        }
        Geom::MultiPoint(pts) => {
            write_u32_le(buf, TY_MULTIPOINT);
            write_count_le(buf, pts.len())?;
            for &(x, y) in pts {
                buf.push(GAIA_MARK_ENTITY);
                write_u32_le(buf, TY_POINT);
                write_xy(buf, x, y);
            }
        }
        Geom::MultiLineString(lines) => {
            write_u32_le(buf, TY_MULTILINESTRING);
            write_count_le(buf, lines.len())?;
            for line in lines {
                buf.push(GAIA_MARK_ENTITY);
                write_u32_le(buf, TY_LINESTRING);
                write_count_le(buf, line.len())?;
                for &(x, y) in line {
                    write_xy(buf, x, y);
                }
            }
        }
        Geom::MultiPolygon(polys) => {
            write_u32_le(buf, TY_MULTIPOLYGON);
            write_count_le(buf, polys.len())?;
            for poly in polys {
                buf.push(GAIA_MARK_ENTITY);
                write_u32_le(buf, TY_POLYGON);
                write_count_le(buf, poly.len())?;
                for ring in poly {
                    write_count_le(buf, ring.len())?;
                    for &(x, y) in ring {
                        write_xy(buf, x, y);
                    }
                }
            }
        }
    }
    Ok(())
}

fn read_inner(c: &mut Cursor<'_>, little: bool) -> Result<Geom> {
    let ty = c.read_u32(little)?;
    match ty {
        TY_POINT => {
            let x = c.read_f64(little)?;
            let y = c.read_f64(little)?;
            Ok(Geom::Point(x, y))
        }
        TY_LINESTRING => {
            let n = c.read_u32(little)? as usize;
            Ok(Geom::LineString(read_pts(c, n, little)?))
        }
        TY_POLYGON => {
            let nrings = c.read_u32(little)? as usize;
            c.ensure_remaining_for(nrings, 4)?;
            let mut rings = Vec::with_capacity(nrings);
            for _ in 0..nrings {
                let np = c.read_u32(little)? as usize;
                rings.push(read_pts(c, np, little)?);
            }
            Ok(Geom::Polygon(rings))
        }
        TY_MULTIPOINT => {
            let n = c.read_u32(little)? as usize;
            c.ensure_remaining_for(n, 1 + 4 + 16)?;
            let mut pts = Vec::with_capacity(n);
            for _ in 0..n {
                expect_entity_marker(c)?;
                let sub = c.read_u32(little)?;
                if sub != TY_POINT {
                    return Err(Error::Geometry(format!(
                        "MultiPoint child must be Point, got type {sub}"
                    )));
                }
                let x = c.read_f64(little)?;
                let y = c.read_f64(little)?;
                pts.push((x, y));
            }
            Ok(Geom::MultiPoint(pts))
        }
        TY_MULTILINESTRING => {
            let n = c.read_u32(little)? as usize;
            c.ensure_remaining_for(n, 1 + 4 + 4)?;
            let mut lines = Vec::with_capacity(n);
            for _ in 0..n {
                expect_entity_marker(c)?;
                let sub = c.read_u32(little)?;
                if sub != TY_LINESTRING {
                    return Err(Error::Geometry(format!(
                        "MultiLineString child must be LineString, got type {sub}"
                    )));
                }
                let np = c.read_u32(little)? as usize;
                lines.push(read_pts(c, np, little)?);
            }
            Ok(Geom::MultiLineString(lines))
        }
        TY_MULTIPOLYGON => {
            let n = c.read_u32(little)? as usize;
            c.ensure_remaining_for(n, 1 + 4 + 4)?;
            let mut polys = Vec::with_capacity(n);
            for _ in 0..n {
                expect_entity_marker(c)?;
                let sub = c.read_u32(little)?;
                if sub != TY_POLYGON {
                    return Err(Error::Geometry(format!(
                        "MultiPolygon child must be Polygon, got type {sub}"
                    )));
                }
                let nrings = c.read_u32(little)? as usize;
                c.ensure_remaining_for(nrings, 4)?;
                let mut rings = Vec::with_capacity(nrings);
                for _ in 0..nrings {
                    let np = c.read_u32(little)? as usize;
                    rings.push(read_pts(c, np, little)?);
                }
                polys.push(rings);
            }
            Ok(Geom::MultiPolygon(polys))
        }
        TY_GEOMETRYCOLLECTION => Err(Error::Geometry(
            "GeometryCollection not supported in SpatiaLite blob (v0.5)".into(),
        )),
        // Z/M 拡張 (1001, 2001, 3001 など) もここで一括拒否。v1.0 以降で対応する。
        other => Err(Error::Geometry(format!(
            "unsupported SpatiaLite geometry type: {other}"
        ))),
    }
}

fn read_pts(c: &mut Cursor<'_>, n: usize, little: bool) -> Result<Vec<(f64, f64)>> {
    c.ensure_remaining_for(n, 16)?;
    let mut pts = Vec::with_capacity(n);
    for _ in 0..n {
        let x = c.read_f64(little)?;
        let y = c.read_f64(little)?;
        pts.push((x, y));
    }
    Ok(pts)
}

fn expect_entity_marker(c: &mut Cursor<'_>) -> Result<()> {
    let m = c.take(1)?[0];
    if m != GAIA_MARK_ENTITY {
        return Err(Error::Geometry(format!(
            "spatialite blob: expected entity marker 0x69, got {m:#x}"
        )));
    }
    Ok(())
}

#[inline]
fn write_u32_le(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

#[inline]
fn write_xy(buf: &mut Vec<u8>, x: f64, y: f64) {
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
}

fn write_count_le(buf: &mut Vec<u8>, n: usize) -> Result<()> {
    let v =
        u32::try_from(n).map_err(|_| Error::Geometry(format!("count exceeds u32::MAX: {n}")))?;
    write_u32_le(buf, v);
    Ok(())
}

fn read_i32(bytes: &[u8], little: bool) -> i32 {
    let arr = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if little {
        i32::from_le_bytes(arr)
    } else {
        i32::from_be_bytes(arr)
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8], pos: usize) -> Self {
        Self { bytes, pos }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn ensure_remaining_for(&self, count: usize, per_item: usize) -> Result<()> {
        let needed = count
            .checked_mul(per_item)
            .ok_or_else(|| Error::Geometry(format!("count overflow: {count} * {per_item}")))?;
        if needed > self.remaining() {
            return Err(Error::Geometry(format!(
                "spatialite blob: count {count} requires {needed} bytes, only {} available",
                self.remaining()
            )));
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| Error::Geometry("cursor overflow".into()))?;
        if end > self.bytes.len() {
            return Err(Error::Geometry(format!(
                "spatialite blob truncated at byte {} (need {n})",
                self.pos
            )));
        }
        let s = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    fn read_u32(&mut self, little: bool) -> Result<u32> {
        let s = self.take(4)?;
        let arr = [s[0], s[1], s[2], s[3]];
        Ok(if little {
            u32::from_le_bytes(arr)
        } else {
            u32::from_be_bytes(arr)
        })
    }

    fn read_f64(&mut self, little: bool) -> Result<f64> {
        let s = self.take(8)?;
        let arr = [s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]];
        Ok(if little {
            f64::from_le_bytes(arr)
        } else {
            f64::from_be_bytes(arr)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(srid: i32, g: &Geom) {
        let wkb_in = wkb::encode(g).unwrap();
        let blob = encode(srid, &wkb_in).unwrap();
        let (back_srid, wkb_out) = decode(&blob).unwrap();
        assert_eq!(back_srid, srid);
        // 標準 WKB レベルで bit-identical (encoder は LE 固定なので往復で同じバイト列)
        assert_eq!(wkb_in, wkb_out);
        // Geom レベルでも一致
        assert_eq!(wkb::decode(&wkb_out).unwrap(), *g);
    }

    #[test]
    fn point_roundtrip() {
        roundtrip(4326, &Geom::Point(135.0, 35.0));
    }

    #[test]
    fn linestring_roundtrip() {
        roundtrip(
            3857,
            &Geom::LineString(vec![(0.0, 0.0), (10.0, 5.0), (20.0, -3.5)]),
        );
    }

    #[test]
    fn polygon_with_hole_roundtrip() {
        roundtrip(
            4326,
            &Geom::Polygon(vec![
                vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0), (0.0, 0.0)],
                vec![(1.0, 1.0), (2.0, 1.0), (2.0, 2.0), (1.0, 2.0), (1.0, 1.0)],
            ]),
        );
    }

    #[test]
    fn multipoint_roundtrip() {
        roundtrip(0, &Geom::MultiPoint(vec![(0.0, 0.0), (1.0, 1.0)]));
    }

    #[test]
    fn multilinestring_roundtrip() {
        roundtrip(
            4326,
            &Geom::MultiLineString(vec![
                vec![(0.0, 0.0), (1.0, 1.0)],
                vec![(2.0, 2.0), (3.0, 3.0), (4.0, 4.0)],
            ]),
        );
    }

    #[test]
    fn multipolygon_roundtrip() {
        roundtrip(
            4326,
            &Geom::MultiPolygon(vec![
                vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
                vec![
                    vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 3.0), (2.0, 2.0)],
                    vec![(2.4, 2.4), (2.6, 2.4), (2.6, 2.6), (2.4, 2.6), (2.4, 2.4)],
                ],
            ]),
        );
    }

    #[test]
    fn mbr_is_written_correctly() {
        let g = Geom::LineString(vec![(1.0, -2.0), (5.0, 3.0), (-3.0, 7.0)]);
        let wkb_in = wkb::encode(&g).unwrap();
        let blob = encode(4326, &wkb_in).unwrap();
        // MBR は header の 6..38 バイト目 (LE)
        let min_x = f64::from_le_bytes(blob[6..14].try_into().unwrap());
        let min_y = f64::from_le_bytes(blob[14..22].try_into().unwrap());
        let max_x = f64::from_le_bytes(blob[22..30].try_into().unwrap());
        let max_y = f64::from_le_bytes(blob[30..38].try_into().unwrap());
        assert!((min_x - -3.0).abs() < f64::EPSILON);
        assert!((min_y - -2.0).abs() < f64::EPSILON);
        assert!((max_x - 5.0).abs() < f64::EPSILON);
        assert!((max_y - 7.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rejects_bad_start_marker() {
        let mut blob = encode(0, &wkb::encode(&Geom::Point(1.0, 2.0)).unwrap()).unwrap();
        blob[0] = 0xAA;
        let err = decode(&blob).unwrap_err();
        assert!(matches!(err, Error::Geometry(msg) if msg.contains("start marker")));
    }

    #[test]
    fn rejects_bad_endian_byte() {
        let mut blob = encode(0, &wkb::encode(&Geom::Point(1.0, 2.0)).unwrap()).unwrap();
        blob[1] = 0xAA;
        let err = decode(&blob).unwrap_err();
        assert!(matches!(err, Error::Geometry(msg) if msg.contains("endian byte")));
    }

    #[test]
    fn rejects_bad_end_marker() {
        let mut blob = encode(0, &wkb::encode(&Geom::Point(1.0, 2.0)).unwrap()).unwrap();
        let last = blob.len() - 1;
        blob[last] = 0xAA;
        let err = decode(&blob).unwrap_err();
        assert!(matches!(err, Error::Geometry(msg) if msg.contains("end marker")));
    }

    #[test]
    fn rejects_geometrycollection() {
        // type=7 を仕込んだ blob を手で作って拒否されることを確認。
        let mut blob = vec![GAIA_BLOB_START, ENDIAN_LE];
        blob.extend_from_slice(&0_i32.to_le_bytes());
        for v in [0.0_f64, 0.0, 0.0, 0.0] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        blob.push(GAIA_MBR_END);
        blob.extend_from_slice(&TY_GEOMETRYCOLLECTION.to_le_bytes());
        blob.push(GAIA_BLOB_END);
        let err = decode(&blob).unwrap_err();
        assert!(matches!(err, Error::Geometry(msg) if msg.contains("GeometryCollection")));
    }

    #[test]
    fn decodes_big_endian_blob() {
        // BE で書いた手作り blob (Point(1.0, 2.0), srid=4326) を decode できる。
        let mut blob = vec![GAIA_BLOB_START, ENDIAN_BE];
        blob.extend_from_slice(&4326_i32.to_be_bytes());
        for v in [1.0_f64, 2.0, 1.0, 2.0] {
            blob.extend_from_slice(&v.to_be_bytes());
        }
        blob.push(GAIA_MBR_END);
        blob.extend_from_slice(&TY_POINT.to_be_bytes());
        blob.extend_from_slice(&1.0_f64.to_be_bytes());
        blob.extend_from_slice(&2.0_f64.to_be_bytes());
        blob.push(GAIA_BLOB_END);
        let (srid, wkb_out) = decode(&blob).unwrap();
        assert_eq!(srid, 4326);
        assert_eq!(wkb::decode(&wkb_out).unwrap(), Geom::Point(1.0, 2.0));
    }
}
