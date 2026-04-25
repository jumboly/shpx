//! WKB (Well-Known Binary, ISO/IEC 13249-3) の encode / decode。
//!
//! v0.1 は XY のみ対応。Z/M 座標は v0.2 以降で拡張する。
//! Encoder は常に little-endian で書き出すが、decoder は LE/BE 双方を読める。
//!
//! バイト構造（OGC SFA 1.2.1 §8.2）:
//!
//! ```text
//! 1 byte  : byte order   (00 = BE, 01 = LE)
//! 4 bytes : geometry type (1=Point, 2=LineString, 3=Polygon,
//!                          4=MultiPoint, 5=MultiLineString, 6=MultiPolygon,
//!                          7=GeometryCollection)
//! ...     : 型別の座標データ
//! ```

use shpx_core::{Error, Result};

/// WKB が表現できる v0.1 サポート対象のジオメトリ enum。
///
/// XY 座標のみを保持する。Z/M は v0.2 以降で別 variant あるいは別フィールドで追加予定。
#[derive(Debug, Clone, PartialEq)]
pub enum Geom {
    Point(f64, f64),
    LineString(Vec<(f64, f64)>),
    /// 外周 + 内周の配列。先頭リングが外周、以降は穴。
    Polygon(Vec<Vec<(f64, f64)>>),
    MultiPoint(Vec<(f64, f64)>),
    MultiLineString(Vec<Vec<(f64, f64)>>),
    MultiPolygon(Vec<Vec<Vec<(f64, f64)>>>),
}

// WKB geometry type コード（ISO/OGC 標準）。
const TY_POINT: u32 = 1;
const TY_LINESTRING: u32 = 2;
const TY_POLYGON: u32 = 3;
const TY_MULTIPOINT: u32 = 4;
const TY_MULTILINESTRING: u32 = 5;
const TY_MULTIPOLYGON: u32 = 6;

const BYTE_ORDER_LE: u8 = 1;
const BYTE_ORDER_BE: u8 = 0;

// バイト幅の定数。アロケーション見積もりと untrusted 入力のサイズ検証で参照する。
const HEADER_SIZE: usize = 5; // byte order (1) + geometry type (4)
const COUNT_SIZE: usize = 4; // u32 カウント
const SUB_HEADER_SIZE: usize = HEADER_SIZE + COUNT_SIZE; // 子 geometry の最小先頭バイト数
const COORD_SIZE: usize = 16; // x (8) + y (8)

/// [`Geom`] を WKB (little-endian) にシリアライズする。
///
/// 要素数が `u32::MAX` を超える geometry は WKB 仕様で表現不能のため [`Error::Geometry`] を返す。
pub fn encode(g: &Geom) -> Result<Vec<u8>> {
    let mut buf = Vec::with_capacity(estimate_size(g));
    encode_into(g, &mut buf)?;
    Ok(buf)
}

fn estimate_size(g: &Geom) -> usize {
    match g {
        Geom::Point(_, _) => HEADER_SIZE + COORD_SIZE,
        Geom::LineString(pts) => HEADER_SIZE + COUNT_SIZE + pts.len() * COORD_SIZE,
        Geom::Polygon(rings) => {
            HEADER_SIZE
                + COUNT_SIZE
                + rings
                    .iter()
                    .map(|r| COUNT_SIZE + r.len() * COORD_SIZE)
                    .sum::<usize>()
        }
        Geom::MultiPoint(pts) => HEADER_SIZE + COUNT_SIZE + pts.len() * (HEADER_SIZE + COORD_SIZE),
        Geom::MultiLineString(lines) => {
            HEADER_SIZE
                + COUNT_SIZE
                + lines
                    .iter()
                    .map(|l| SUB_HEADER_SIZE + l.len() * COORD_SIZE)
                    .sum::<usize>()
        }
        Geom::MultiPolygon(polys) => {
            HEADER_SIZE
                + COUNT_SIZE
                + polys
                    .iter()
                    .map(|p| {
                        SUB_HEADER_SIZE
                            + p.iter()
                                .map(|r| COUNT_SIZE + r.len() * COORD_SIZE)
                                .sum::<usize>()
                    })
                    .sum::<usize>()
        }
    }
}

fn encode_into(g: &Geom, buf: &mut Vec<u8>) -> Result<()> {
    match g {
        Geom::Point(x, y) => encode_point_into(*x, *y, buf),
        Geom::LineString(pts) => encode_linestring_into(pts, buf)?,
        Geom::Polygon(rings) => encode_polygon_into(rings, buf)?,
        Geom::MultiPoint(pts) => {
            write_header(buf, TY_MULTIPOINT);
            write_count(buf, pts.len())?;
            for (x, y) in pts {
                encode_point_into(*x, *y, buf);
            }
        }
        Geom::MultiLineString(lines) => {
            write_header(buf, TY_MULTILINESTRING);
            write_count(buf, lines.len())?;
            for line in lines {
                encode_linestring_into(line, buf)?;
            }
        }
        Geom::MultiPolygon(polys) => {
            write_header(buf, TY_MULTIPOLYGON);
            write_count(buf, polys.len())?;
            for poly in polys {
                encode_polygon_into(poly, buf)?;
            }
        }
    }
    Ok(())
}

fn encode_point_into(x: f64, y: f64, buf: &mut Vec<u8>) {
    write_header(buf, TY_POINT);
    write_xy(buf, x, y);
}

fn encode_linestring_into(pts: &[(f64, f64)], buf: &mut Vec<u8>) -> Result<()> {
    write_header(buf, TY_LINESTRING);
    write_count(buf, pts.len())?;
    for (x, y) in pts {
        write_xy(buf, *x, *y);
    }
    Ok(())
}

fn encode_polygon_into(rings: &[Vec<(f64, f64)>], buf: &mut Vec<u8>) -> Result<()> {
    write_header(buf, TY_POLYGON);
    write_count(buf, rings.len())?;
    for ring in rings {
        write_count(buf, ring.len())?;
        for (x, y) in ring {
            write_xy(buf, *x, *y);
        }
    }
    Ok(())
}

#[inline]
fn write_header(buf: &mut Vec<u8>, ty: u32) {
    buf.push(BYTE_ORDER_LE);
    write_u32(buf, ty);
}

#[inline]
fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

#[inline]
fn write_xy(buf: &mut Vec<u8>, x: f64, y: f64) {
    buf.extend_from_slice(&x.to_le_bytes());
    buf.extend_from_slice(&y.to_le_bytes());
}

/// `usize` を WKB の u32 カウントとして書き込む。
/// `u32::MAX` を超える要素数は WKB 仕様で表現不能なのでエラーで返す（無音破損を避ける）。
fn write_count(buf: &mut Vec<u8>, n: usize) -> Result<()> {
    let v =
        u32::try_from(n).map_err(|_| Error::Geometry(format!("count exceeds u32::MAX: {n}")))?;
    write_u32(buf, v);
    Ok(())
}

/// WKB バイト列を [`Geom`] にデシリアライズする。LE / BE 両対応。
pub fn decode(bytes: &[u8]) -> Result<Geom> {
    let mut c = Cursor::new(bytes);
    let g = read_geom(&mut c)?;
    if c.pos != bytes.len() {
        // 余りバイトがある場合は不正データとしてエラーにする。
        return Err(Error::Geometry(format!(
            "trailing bytes after WKB ({} bytes left)",
            bytes.len() - c.pos
        )));
    }
    Ok(g)
}

fn read_geom(c: &mut Cursor<'_>) -> Result<Geom> {
    let order = c.take(1)?[0];
    let little = match order {
        BYTE_ORDER_LE => true,
        BYTE_ORDER_BE => false,
        other => {
            return Err(Error::Geometry(format!(
                "invalid WKB byte order: {other:#x}"
            )));
        }
    };
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
            // untrusted な nrings での過大アロケーション抑止。
            // 各リングは少なくとも `COUNT_SIZE` バイト必要。
            c.ensure_remaining_for(nrings, COUNT_SIZE)?;
            let mut rings = Vec::with_capacity(nrings);
            for _ in 0..nrings {
                let np = c.read_u32(little)? as usize;
                rings.push(read_pts(c, np, little)?);
            }
            Ok(Geom::Polygon(rings))
        }
        TY_MULTIPOINT => {
            let n = c.read_u32(little)? as usize;
            // 各子 Point は `HEADER_SIZE + COORD_SIZE` バイト。
            c.ensure_remaining_for(n, HEADER_SIZE + COORD_SIZE)?;
            let mut pts = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geom(c)? {
                    Geom::Point(x, y) => pts.push((x, y)),
                    other => {
                        return Err(Error::Geometry(format!(
                            "MultiPoint child must be Point, got {other:?}"
                        )));
                    }
                }
            }
            Ok(Geom::MultiPoint(pts))
        }
        TY_MULTILINESTRING => {
            let n = c.read_u32(little)? as usize;
            // 各子 LineString は最低 `SUB_HEADER_SIZE` バイト（要素 0 でも先頭ヘッダ + count）。
            c.ensure_remaining_for(n, SUB_HEADER_SIZE)?;
            let mut lines = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geom(c)? {
                    Geom::LineString(pts) => lines.push(pts),
                    other => {
                        return Err(Error::Geometry(format!(
                            "MultiLineString child must be LineString, got {other:?}"
                        )));
                    }
                }
            }
            Ok(Geom::MultiLineString(lines))
        }
        TY_MULTIPOLYGON => {
            let n = c.read_u32(little)? as usize;
            // 各子 Polygon は最低 `SUB_HEADER_SIZE` バイト。
            c.ensure_remaining_for(n, SUB_HEADER_SIZE)?;
            let mut polys = Vec::with_capacity(n);
            for _ in 0..n {
                match read_geom(c)? {
                    Geom::Polygon(rings) => polys.push(rings),
                    other => {
                        return Err(Error::Geometry(format!(
                            "MultiPolygon child must be Polygon, got {other:?}"
                        )));
                    }
                }
            }
            Ok(Geom::MultiPolygon(polys))
        }
        other => Err(Error::Geometry(format!("unsupported WKB type: {other}"))),
    }
}

fn read_pts(c: &mut Cursor<'_>, n: usize, little: bool) -> Result<Vec<(f64, f64)>> {
    // untrusted な n でアロケーションする前に、残バイトに収まることを確認する（DoS 抑止）。
    c.ensure_remaining_for(n, COORD_SIZE)?;
    let mut pts = Vec::with_capacity(n);
    for _ in 0..n {
        let x = c.read_f64(little)?;
        let y = c.read_f64(little)?;
        pts.push((x, y));
    }
    Ok(pts)
}

/// バイトスライス上のカーソル。バウンダリチェック付きで `take()` する小さなヘルパ。
struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    /// `count * per_item` バイトが残量に収まるかを事前検証する。
    /// untrusted な `count` を [`Vec::with_capacity`] へ渡す前に呼ぶことで、
    /// 巨大値による過大アロケーション（DoS）を防ぐ。
    fn ensure_remaining_for(&self, count: usize, per_item: usize) -> Result<()> {
        let needed = count
            .checked_mul(per_item)
            .ok_or_else(|| Error::Geometry(format!("count overflow: {count} * {per_item}")))?;
        if needed > self.remaining() {
            return Err(Error::Geometry(format!(
                "count {count} requires {needed} bytes, only {} available",
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
                "WKB truncated at byte {} (need {n})",
                self.pos
            )));
        }
        let slice = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(slice)
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

    fn roundtrip(g: &Geom) {
        let bytes = encode(g).unwrap();
        let back = decode(&bytes).unwrap();
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
    fn rejects_invalid_byte_order() {
        let bad = [0x02, 0x01, 0x00, 0x00, 0x00];
        let err = decode(&bad).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("byte order")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_truncated_input() {
        // Point は 21 バイト必要だが、12 バイトしか与えない。
        let bytes = encode(&Geom::Point(1.0, 2.0)).unwrap();
        let truncated = &bytes[..12];
        let err = decode(truncated).unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }

    #[test]
    fn rejects_trailing_bytes() {
        let mut bytes = encode(&Geom::Point(1.0, 2.0)).unwrap();
        bytes.push(0xFF);
        let err = decode(&bytes).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("trailing")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn decodes_big_endian_point() {
        // OGC のサンプル: byte order 0 (BE), type=1 (Point), x=1.0, y=2.0
        let mut bytes = vec![0x00];
        bytes.extend_from_slice(&1_u32.to_be_bytes());
        bytes.extend_from_slice(&1.0_f64.to_be_bytes());
        bytes.extend_from_slice(&2.0_f64.to_be_bytes());
        assert_eq!(decode(&bytes).unwrap(), Geom::Point(1.0, 2.0));
    }

    #[test]
    fn rejects_oversized_point_count() {
        // LineString のヘッダ部に巨大な n を仕込み、残バイト不足で拒否されること。
        let mut bytes = vec![BYTE_ORDER_LE];
        bytes.extend_from_slice(&TY_LINESTRING.to_le_bytes());
        // n = u32::MAX。実バイトは coord 1 つ分も無い。
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        let err = decode(&bytes).unwrap_err();
        match err {
            Error::Geometry(msg) => {
                assert!(
                    msg.contains("count") || msg.contains("requires"),
                    "got: {msg}"
                );
            }
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }
}
