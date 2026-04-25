//! WKT (Well-Known Text, OGC SFA 1.2.1) の encode / decode。
//!
//! v0.2 サイクル 1 では XY のみ対応。Z/M / EMPTY は未サポートで、
//! `EMPTY` を含む入力は `Error::Geometry` を返す（`Geom` enum を v0.2 中に
//! 拡張するのを避けるため。回避策は `--on-loss=skip` を案内）。
//!
//! Encoder は数値出力に Rust 既定の `Display` を使うため、`f64` を最短の
//! round-trip 表現で書く。固定桁フォーマット (`{:.10}` 等) を使うと `1.0` が
//! `1.0000000000` のように長大化するため避けている。
//!
//! Parser は手書きトークナイザで実装する。`regex` を使うとエラー位置が
//! 曖昧になり、巨大 untrusted 入力で複雑なバックトラックが発生し得るため。

use shpx_core::{Error, Result};

use crate::wkb::Geom;

/// [`Geom`] を WKT 文字列にシリアライズする。
///
/// 現状は失敗しないが、Z/M / EMPTY 等の追加サポート時に
/// `Result` を返したくなる（ジオメトリ整合性チェック等）ため、
/// API シグネチャを `Result` のまま固定しておく。
pub fn encode(g: &Geom) -> Result<String> {
    let mut s = String::new();
    encode_into(g, &mut s);
    Ok(s)
}

fn encode_into(g: &Geom, s: &mut String) {
    match g {
        Geom::Point(x, y) => {
            s.push_str("POINT(");
            write_xy(s, *x, *y);
            s.push(')');
        }
        Geom::LineString(pts) => {
            s.push_str("LINESTRING");
            write_pts(s, pts);
        }
        Geom::Polygon(rings) => {
            s.push_str("POLYGON");
            write_rings(s, rings);
        }
        Geom::MultiPoint(pts) => {
            // OGC SFA 推奨は `MULTIPOINT((x y),(x y))` 形式。GDAL/PostGIS の双方が読める。
            s.push_str("MULTIPOINT(");
            for (i, (x, y)) in pts.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                s.push('(');
                write_xy(s, *x, *y);
                s.push(')');
            }
            s.push(')');
        }
        Geom::MultiLineString(lines) => {
            s.push_str("MULTILINESTRING(");
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write_pts(s, line);
            }
            s.push(')');
        }
        Geom::MultiPolygon(polys) => {
            s.push_str("MULTIPOLYGON(");
            for (i, rings) in polys.iter().enumerate() {
                if i > 0 {
                    s.push(',');
                }
                write_rings(s, rings);
            }
            s.push(')');
        }
    }
}

fn write_xy(s: &mut String, x: f64, y: f64) {
    use std::fmt::Write;
    // `Display` は IEEE-754 最短の round-trip 表現を返す。
    let _ = write!(s, "{x} {y}");
}

fn write_pts(s: &mut String, pts: &[(f64, f64)]) {
    s.push('(');
    for (i, (x, y)) in pts.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        write_xy(s, *x, *y);
    }
    s.push(')');
}

fn write_rings(s: &mut String, rings: &[Vec<(f64, f64)>]) {
    s.push('(');
    for (i, ring) in rings.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        write_pts(s, ring);
    }
    s.push(')');
}

/// WKT 文字列を [`Geom`] にデシリアライズする。
pub fn decode(s: &str) -> Result<Geom> {
    let mut p = Parser::new(s);
    p.skip_ws();
    let g = p.parse_geom()?;
    p.skip_ws();
    if !p.eof() {
        return Err(geom_err(format!(
            "trailing characters after geometry at position {}",
            p.pos
        )));
    }
    Ok(g)
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(s: &'a str) -> Self {
        Self {
            src: s.as_bytes(),
            pos: 0,
        }
    }

    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if b == b' ' || b == b'\t' || b == b'\n' || b == b'\r' {
                self.pos += 1;
            } else {
                break;
            }
        }
    }

    fn expect(&mut self, c: u8) -> Result<()> {
        self.skip_ws();
        match self.peek() {
            Some(b) if b == c => {
                self.pos += 1;
                Ok(())
            }
            Some(b) => Err(geom_err(format!(
                "expected '{}' at position {}, found '{}'",
                c as char, self.pos, b as char
            ))),
            None => Err(geom_err(format!(
                "expected '{}' at position {}, found EOF",
                c as char, self.pos
            ))),
        }
    }

    /// 識別子（大文字英字）を読む。
    fn read_ident(&mut self) -> Result<String> {
        self.skip_ws();
        let start = self.pos;
        while let Some(b) = self.peek() {
            if b.is_ascii_alphabetic() {
                self.pos += 1;
            } else {
                break;
            }
        }
        if self.pos == start {
            return Err(geom_err(format!(
                "expected geometry keyword at position {start}"
            )));
        }
        let raw = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| geom_err("non-UTF-8 in geometry keyword"))?;
        Ok(raw.to_ascii_uppercase())
    }

    /// 数値リテラル 1 つ読む（`f64::parse` に委譲）。
    fn read_number(&mut self) -> Result<f64> {
        self.skip_ws();
        let start = self.pos;
        // 符号
        if matches!(self.peek(), Some(b'+' | b'-')) {
            self.pos += 1;
        }
        // 整数部 / 小数部
        while let Some(b) = self.peek() {
            if b.is_ascii_digit() || b == b'.' {
                self.pos += 1;
            } else {
                break;
            }
        }
        // 指数部
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            while let Some(b) = self.peek() {
                if b.is_ascii_digit() {
                    self.pos += 1;
                } else {
                    break;
                }
            }
        }
        if self.pos == start {
            return Err(geom_err(format!("expected number at position {start}")));
        }
        let raw = std::str::from_utf8(&self.src[start..self.pos])
            .map_err(|_| geom_err("non-UTF-8 in number"))?;
        raw.parse::<f64>()
            .map_err(|e| geom_err(format!("invalid number `{raw}` at position {start}: {e}")))
    }

    fn read_xy(&mut self) -> Result<(f64, f64)> {
        let x = self.read_number()?;
        let y = self.read_number()?;
        Ok((x, y))
    }

    /// 直後のトークンが大文字小文字無視で `kw` に一致すれば消費して true。
    fn consume_keyword(&mut self, kw: &str) -> bool {
        self.skip_ws();
        let bytes = kw.as_bytes();
        if self.pos + bytes.len() > self.src.len() {
            return false;
        }
        let region = &self.src[self.pos..self.pos + bytes.len()];
        if !region.eq_ignore_ascii_case(bytes) {
            return false;
        }
        // 続く文字が英字なら部分一致なので NG（`POINTZ` を `POINT` と誤認しない）。
        let after = self.pos + bytes.len();
        if let Some(b) = self.src.get(after) {
            if b.is_ascii_alphabetic() {
                return false;
            }
        }
        self.pos = after;
        true
    }

    fn reject_empty(&mut self) -> Result<()> {
        if self.consume_keyword("EMPTY") {
            return Err(geom_err(
                "WKT EMPTY geometries are not supported in v0.2 (use --on-loss=skip to drop rows)",
            ));
        }
        Ok(())
    }

    fn parse_geom(&mut self) -> Result<Geom> {
        let kw = self.read_ident()?;
        match kw.as_str() {
            "POINT" => self.parse_point(),
            "LINESTRING" => self.parse_linestring(),
            "POLYGON" => self.parse_polygon(),
            "MULTIPOINT" => self.parse_multipoint(),
            "MULTILINESTRING" => self.parse_multilinestring(),
            "MULTIPOLYGON" => self.parse_multipolygon(),
            other => Err(geom_err(format!(
                "unsupported WKT geometry keyword `{other}` (only XY POINT/LINESTRING/POLYGON/MULTI* in v0.2)"
            ))),
        }
    }

    fn parse_point(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        self.expect(b'(')?;
        let (x, y) = self.read_xy()?;
        self.expect(b')')?;
        Ok(Geom::Point(x, y))
    }

    fn parse_pts_list(&mut self) -> Result<Vec<(f64, f64)>> {
        self.expect(b'(')?;
        let mut pts = Vec::new();
        loop {
            pts.push(self.read_xy()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(geom_err(format!(
                        "expected ',' or ')' at position {}, found '{}'",
                        self.pos, b as char
                    )));
                }
                None => return Err(geom_err("unexpected EOF in coordinate list")),
            }
        }
        Ok(pts)
    }

    fn parse_linestring(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        Ok(Geom::LineString(self.parse_pts_list()?))
    }

    fn parse_polygon(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        Ok(Geom::Polygon(self.parse_rings()?))
    }

    fn parse_rings(&mut self) -> Result<Vec<Vec<(f64, f64)>>> {
        self.expect(b'(')?;
        let mut rings = Vec::new();
        loop {
            rings.push(self.parse_pts_list()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(geom_err(format!(
                        "expected ',' or ')' at position {}, found '{}'",
                        self.pos, b as char
                    )));
                }
                None => return Err(geom_err("unexpected EOF in ring list")),
            }
        }
        Ok(rings)
    }

    fn parse_multipoint(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        // MULTIPOINT は (1 2, 3 4) と ((1 2),(3 4)) の双方を受け付ける（GDAL/PostGIS 互換）。
        self.expect(b'(')?;
        let mut pts = Vec::new();
        loop {
            self.skip_ws();
            let (x, y) = if self.peek() == Some(b'(') {
                self.pos += 1;
                let p = self.read_xy()?;
                self.expect(b')')?;
                p
            } else {
                self.read_xy()?
            };
            pts.push((x, y));
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(geom_err(format!(
                        "expected ',' or ')' at position {}, found '{}'",
                        self.pos, b as char
                    )));
                }
                None => return Err(geom_err("unexpected EOF in MULTIPOINT")),
            }
        }
        Ok(Geom::MultiPoint(pts))
    }

    fn parse_multilinestring(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        self.expect(b'(')?;
        let mut lines = Vec::new();
        loop {
            lines.push(self.parse_pts_list()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(geom_err(format!(
                        "expected ',' or ')' at position {}, found '{}'",
                        self.pos, b as char
                    )));
                }
                None => return Err(geom_err("unexpected EOF in MULTILINESTRING")),
            }
        }
        Ok(Geom::MultiLineString(lines))
    }

    fn parse_multipolygon(&mut self) -> Result<Geom> {
        self.reject_empty()?;
        self.expect(b'(')?;
        let mut polys = Vec::new();
        loop {
            polys.push(self.parse_rings()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b) => {
                    return Err(geom_err(format!(
                        "expected ',' or ')' at position {}, found '{}'",
                        self.pos, b as char
                    )));
                }
                None => return Err(geom_err("unexpected EOF in MULTIPOLYGON")),
            }
        }
        Ok(Geom::MultiPolygon(polys))
    }
}

fn geom_err(msg: impl Into<String>) -> Error {
    Error::Geometry(msg.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(g: &Geom) {
        let s = encode(g).unwrap();
        let back = decode(&s).unwrap();
        assert_eq!(*g, back, "roundtrip mismatch for `{s}`");
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
            vec![(2.0, 2.0), (3.0, 3.0)],
        ]));
    }

    #[test]
    fn multipolygon_roundtrip() {
        roundtrip(&Geom::MultiPolygon(vec![
            vec![vec![(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)]],
            vec![vec![(2.0, 2.0), (3.0, 2.0), (3.0, 3.0), (2.0, 2.0)]],
        ]));
    }

    #[test]
    fn multipoint_accepts_flat_form() {
        // GDAL/PostGIS が出す `MULTIPOINT(1 2, 3 4)` 形式（括弧なし member）も受け付ける。
        let g = decode("MULTIPOINT(1 2, 3 4)").unwrap();
        assert_eq!(g, Geom::MultiPoint(vec![(1.0, 2.0), (3.0, 4.0)]));
    }

    #[test]
    fn allows_extra_whitespace() {
        let g = decode("  POINT (\n  1.0   2.0\n) \t").unwrap();
        assert_eq!(g, Geom::Point(1.0, 2.0));
    }

    #[test]
    fn case_insensitive_keyword() {
        let g = decode("point(1 2)").unwrap();
        assert_eq!(g, Geom::Point(1.0, 2.0));
    }

    #[test]
    fn rejects_empty() {
        let err = decode("POINT EMPTY").unwrap_err();
        assert!(matches!(err, Error::Geometry(ref m) if m.contains("EMPTY")));
    }

    #[test]
    fn rejects_unknown_keyword() {
        let err = decode("CIRCLE(1 2, 3)").unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }

    #[test]
    fn rejects_trailing_garbage() {
        let err = decode("POINT(1 2) garbage").unwrap_err();
        assert!(matches!(err, Error::Geometry(ref m) if m.contains("trailing")));
    }

    #[test]
    fn rejects_partial_keyword_match() {
        // `POINTZ` を `POINT` と誤認させない（v0.2 では Z は未サポート → unknown keyword）。
        let err = decode("POINTZ(1 2 3)").unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }
}
