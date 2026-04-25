//! PostGIS EWKB (Extended WKB) の encode / decode。
//!
//! EWKB は PostGIS が WKB を拡張したバイナリ形式で、geometry type フィールドの
//! 上位ビットに以下の拡張 flag を持つ:
//!
//! - `0x80000000` = Z 座標あり
//! - `0x40000000` = M 座標あり
//! - `0x20000000` = SRID 埋め込みあり（type 直後に i32 で SRID が続く）
//!
//! Z/M 座標は `shpx-geom::wkb` 自体が未対応のため、Z/M flag が立った EWKB は
//! [`Error::Geometry`] で拒否する。SRID flag のみを扱い、それ以外の bytes は
//! 標準 WKB と同じレイアウトで素通しする（子 geometry には SRID を埋めない PostGIS の慣習に従う）。
//!
//! 参照: <https://postgis.net/docs/ST_AsEWKB.html>、<https://libgeos.org/specifications/wkb/#extended-wkb>

use shpx_core::{Error, Result};

use crate::wkb;

/// EWKB の SRID 埋め込み flag。type フィールドの上位ビットに OR されている。
const SRID_FLAG: u32 = 0x2000_0000;
/// EWKB の Z 座標 flag（v0.3 cycle 1 では未対応）。
const Z_FLAG: u32 = 0x8000_0000;
/// EWKB の M 座標 flag（v0.3 cycle 1 では未対応）。
const M_FLAG: u32 = 0x4000_0000;

const BYTE_ORDER_LE: u8 = 1;
const BYTE_ORDER_BE: u8 = 0;

/// 標準 WKB に SRID を埋め込んで EWKB に変換する。
///
/// WKB のトップレベル type フィールドに [`SRID_FLAG`] を立て、type 直後に SRID を i32 で挿入する。
/// 子 geometry（MultiPoint の各 Point など）は触らない。
///
/// # Errors
/// - 入力 WKB が 5 バイト未満
/// - byte order が 0/1 以外
/// - 入力 WKB に既に Z/M/SRID flag が立っている（SRID 二重付与と Z/M は未対応として拒否）
pub fn encode_with_srid(wkb_bytes: &[u8], srid: i32) -> Result<Vec<u8>> {
    if wkb_bytes.len() < 5 {
        return Err(Error::Geometry(format!(
            "WKB too short for EWKB header: {} bytes",
            wkb_bytes.len()
        )));
    }
    let order = wkb_bytes[0];
    let little = match order {
        BYTE_ORDER_LE => true,
        BYTE_ORDER_BE => false,
        other => {
            return Err(Error::Geometry(format!(
                "invalid WKB byte order: {other:#x}"
            )));
        }
    };
    let ty = read_type(wkb_bytes, little);
    if ty & SRID_FLAG != 0 {
        return Err(Error::Geometry(
            "input already has SRID flag; refusing to double-encode".into(),
        ));
    }
    if ty & (Z_FLAG | M_FLAG) != 0 {
        return Err(Error::Geometry(
            "Z/M-coordinate WKB is not supported".into(),
        ));
    }
    let new_ty = ty | SRID_FLAG;
    let mut out = Vec::with_capacity(wkb_bytes.len() + 4);
    out.push(order);
    write_u32(&mut out, new_ty, little);
    write_i32(&mut out, srid, little);
    out.extend_from_slice(&wkb_bytes[5..]);
    Ok(out)
}

/// EWKB から SRID を取り出し、標準 WKB に戻す。
///
/// SRID flag が立っていなければ入力をそのまま標準 WKB として返し、SRID は `None`。
/// Z/M flag が立っている入力は cycle 1 では未対応として拒否する。
pub fn strip_srid(ewkb: &[u8]) -> Result<(Vec<u8>, Option<i32>)> {
    if ewkb.len() < 5 {
        return Err(Error::Geometry(format!(
            "EWKB too short for header: {} bytes",
            ewkb.len()
        )));
    }
    let order = ewkb[0];
    let little = match order {
        BYTE_ORDER_LE => true,
        BYTE_ORDER_BE => false,
        other => {
            return Err(Error::Geometry(format!(
                "invalid EWKB byte order: {other:#x}"
            )));
        }
    };
    let ty = read_type(ewkb, little);
    if ty & (Z_FLAG | M_FLAG) != 0 {
        return Err(Error::Geometry(
            "Z/M-coordinate EWKB is not supported".into(),
        ));
    }
    if ty & SRID_FLAG == 0 {
        // 標準 WKB として透過: そのままコピーする。
        return Ok((ewkb.to_vec(), None));
    }
    if ewkb.len() < 9 {
        return Err(Error::Geometry(
            "EWKB has SRID flag but is truncated before SRID field".into(),
        ));
    }
    let base_ty = ty & !SRID_FLAG;
    let srid = read_i32(&ewkb[5..9], little);
    let mut out = Vec::with_capacity(ewkb.len() - 4);
    out.push(order);
    write_u32(&mut out, base_ty, little);
    out.extend_from_slice(&ewkb[9..]);
    Ok((out, Some(srid)))
}

/// EWKB を [`wkb::Geom`] と SRID に分解する。
///
/// 内部で [`strip_srid`] → [`wkb::decode`] を呼ぶだけの薄いラッパ。
pub fn decode(ewkb: &[u8]) -> Result<(wkb::Geom, Option<i32>)> {
    let (wkb_bytes, srid) = strip_srid(ewkb)?;
    let g = wkb::decode(&wkb_bytes)?;
    Ok((g, srid))
}

#[inline]
fn read_type(bytes: &[u8], little: bool) -> u32 {
    let arr = [bytes[1], bytes[2], bytes[3], bytes[4]];
    if little {
        u32::from_le_bytes(arr)
    } else {
        u32::from_be_bytes(arr)
    }
}

#[inline]
fn read_i32(bytes: &[u8], little: bool) -> i32 {
    let arr = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if little {
        i32::from_le_bytes(arr)
    } else {
        i32::from_be_bytes(arr)
    }
}

#[inline]
fn write_u32(buf: &mut Vec<u8>, v: u32, little: bool) {
    let bytes = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    buf.extend_from_slice(&bytes);
}

#[inline]
fn write_i32(buf: &mut Vec<u8>, v: i32, little: bool) {
    let bytes = if little {
        v.to_le_bytes()
    } else {
        v.to_be_bytes()
    };
    buf.extend_from_slice(&bytes);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wkb::Geom;

    fn point_wkb(x: f64, y: f64) -> Vec<u8> {
        wkb::encode(&Geom::Point(x, y)).unwrap()
    }

    #[test]
    fn encode_with_srid_then_strip_roundtrips_to_input_wkb() {
        let wkb_in = point_wkb(1.0, 2.0);
        let ewkb = encode_with_srid(&wkb_in, 4326).unwrap();
        // EWKB は WKB より 4 バイト長い（SRID 分）。
        assert_eq!(ewkb.len(), wkb_in.len() + 4);
        // SRID flag が立っていることを確認。
        let ty = u32::from_le_bytes([ewkb[1], ewkb[2], ewkb[3], ewkb[4]]);
        assert_ne!(ty & SRID_FLAG, 0);
        // SRID 値を確認。
        let srid_in_ewkb = i32::from_le_bytes([ewkb[5], ewkb[6], ewkb[7], ewkb[8]]);
        assert_eq!(srid_in_ewkb, 4326);

        let (wkb_back, srid_back) = strip_srid(&ewkb).unwrap();
        assert_eq!(wkb_back, wkb_in);
        assert_eq!(srid_back, Some(4326));
    }

    #[test]
    fn strip_srid_passes_standard_wkb_through() {
        let wkb_in = point_wkb(3.0, 4.0);
        let (wkb_back, srid) = strip_srid(&wkb_in).unwrap();
        assert_eq!(wkb_back, wkb_in);
        assert_eq!(srid, None);
    }

    #[test]
    fn decode_yields_geom_and_srid() {
        let wkb_in = point_wkb(5.0, -6.0);
        let ewkb = encode_with_srid(&wkb_in, 3857).unwrap();
        let (g, srid) = decode(&ewkb).unwrap();
        assert_eq!(g, Geom::Point(5.0, -6.0));
        assert_eq!(srid, Some(3857));
    }

    #[test]
    fn rejects_too_short_input() {
        assert!(matches!(
            encode_with_srid(&[0x01, 0x01], 4326),
            Err(Error::Geometry(_))
        ));
        assert!(matches!(strip_srid(&[0x01]), Err(Error::Geometry(_))));
    }

    #[test]
    fn rejects_invalid_byte_order() {
        let bad = [0x02, 0x01, 0x00, 0x00, 0x00];
        assert!(matches!(
            encode_with_srid(&bad, 4326),
            Err(Error::Geometry(_))
        ));
        assert!(matches!(strip_srid(&bad), Err(Error::Geometry(_))));
    }

    #[test]
    fn rejects_double_srid_encoding() {
        let wkb_in = point_wkb(1.0, 2.0);
        let ewkb = encode_with_srid(&wkb_in, 4326).unwrap();
        // 2 度目の encode_with_srid は SRID flag 衝突で拒否。
        assert!(matches!(
            encode_with_srid(&ewkb, 3857),
            Err(Error::Geometry(_))
        ));
    }

    #[test]
    fn rejects_z_or_m_input() {
        // type=1 (Point) に Z flag を立てた偽 WKB。
        let mut bad = vec![BYTE_ORDER_LE];
        bad.extend_from_slice(&(1_u32 | Z_FLAG).to_le_bytes());
        bad.extend_from_slice(&[0u8; 16]); // x, y
        assert!(matches!(
            encode_with_srid(&bad, 4326),
            Err(Error::Geometry(_))
        ));
    }

    #[test]
    fn handles_big_endian_ewkb() {
        // BE で組んだ EWKB を strip_srid に通す。
        let mut be_ewkb = vec![BYTE_ORDER_BE];
        be_ewkb.extend_from_slice(&(1_u32 | SRID_FLAG).to_be_bytes());
        be_ewkb.extend_from_slice(&4326_i32.to_be_bytes());
        be_ewkb.extend_from_slice(&1.0_f64.to_be_bytes());
        be_ewkb.extend_from_slice(&2.0_f64.to_be_bytes());
        let (wkb_back, srid) = strip_srid(&be_ewkb).unwrap();
        assert_eq!(srid, Some(4326));
        assert_eq!(wkb::decode(&wkb_back).unwrap(), Geom::Point(1.0, 2.0));
    }

    #[test]
    fn srid_truncated_returns_error() {
        // SRID flag が立っているのに SRID 4 バイトが足りない。
        let mut bad = vec![BYTE_ORDER_LE];
        bad.extend_from_slice(&(1_u32 | SRID_FLAG).to_le_bytes());
        // SRID は 0 バイトしか書かない。
        let err = strip_srid(&bad).unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }

    #[test]
    fn negative_srid_roundtrips() {
        // PostGIS は SRID に負値（unknown CRS のマーカ等）を受け付けることがある。
        let wkb_in = point_wkb(0.0, 0.0);
        let ewkb = encode_with_srid(&wkb_in, -1).unwrap();
        let (_, srid) = strip_srid(&ewkb).unwrap();
        assert_eq!(srid, Some(-1));
    }
}
