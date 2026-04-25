//! GeoPackage Binary geometry header (OGC GeoPackage 1.3 §2.1.3.1.1) の encode/decode。
//!
//! GPKG 仕様で feature テーブルの geometry 列に格納される BLOB は
//! 「StandardGeoPackageBinary」: 独自ヘッダ + 標準 WKB ペイロードの構造を持つ。
//!
//! # ヘッダレイアウト
//!
//! ```text
//! offset size 内容
//! 0      2    magic = 0x47 0x50 ("GP")
//! 2      1    version = 0
//! 3      1    flags : bit0=endian (1=LE) | bit1-3=envelope_type | bit4=empty | bit5=binary_type
//! 4      4    srs_id (i32, header の endian)
//! 8      ...  envelope (envelope_type に応じて 0/32/48/48/64 bytes)
//! 末尾   ..   標準 WKB ペイロード（WKB 内部の byte order に従う）
//! ```
//!
//! # スコープ
//!
//! - encode は version=0 / endian=LE / binary_type=Standard / envelope=None 固定。
//! - decode は LE/BE 双方、全 envelope_type (0/1/2/3/4) と Extended binary_type を受け付ける
//!   （envelope は内容のみ読み飛ばす。空間検索を実装するまで活用しない）。
//!
//! ヘッダ仕様の詳細は <https://www.geopackage.org/spec130/index.html#gpb_format> を参照。

use shpx_core::{Error, Result};

/// GeoPackage geometry blob の magic bytes (`"GP"`)。
pub const MAGIC: [u8; 2] = [0x47, 0x50];

/// 現在サポートする version。仕様上の唯一の値で、reader でも本値以外は拒否する。
pub const VERSION: u8 = 0;

const FLAG_ENDIAN_LE: u8 = 0b0000_0001;
const FLAG_EMPTY: u8 = 0b0001_0000;
const FLAG_BINARY_EXTENDED: u8 = 0b0010_0000;
const FLAG_ENVELOPE_MASK: u8 = 0b0000_1110;
const FLAG_ENVELOPE_SHIFT: u8 = 1;

/// BBOX を表す envelope。flags の `envelope_type` (0..=4) と 1:1 対応。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Envelope {
    /// envelope を持たない（type=0）。
    None,
    /// XY: `[min_x, max_x, min_y, max_y]`（type=1, 32 bytes）。
    Xy([f64; 4]),
    /// XYZ: `[min_x, max_x, min_y, max_y, min_z, max_z]`（type=2, 48 bytes）。
    Xyz([f64; 6]),
    /// XYM: `[min_x, max_x, min_y, max_y, min_m, max_m]`（type=3, 48 bytes）。
    Xym([f64; 6]),
    /// XYZM: `[min_x, max_x, min_y, max_y, min_z, max_z, min_m, max_m]`（type=4, 64 bytes）。
    Xyzm([f64; 8]),
}

impl Envelope {
    fn type_code(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Xy(_) => 1,
            Self::Xyz(_) => 2,
            Self::Xym(_) => 3,
            Self::Xyzm(_) => 4,
        }
    }

    fn byte_len(self) -> usize {
        envelope_byte_len(self.type_code())
    }
}

/// blob ペイロードの種別。Extended は GPKG 拡張で `gpkg_extensions` に登録する非標準ジオメトリ。
/// shpx は v0.2 では Standard のみ書き出すが、reader は Extended も透過に読める。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryType {
    Standard,
    Extended,
}

/// GeoPackage geometry blob の header 部。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GpkgBlobHeader {
    pub srs_id: i32,
    pub envelope: Envelope,
    pub empty: bool,
    pub binary_type: BinaryType,
}

impl GpkgBlobHeader {
    /// envelope 無しで standard binary を作る簡易コンストラクタ。
    #[must_use]
    pub fn standard(srs_id: i32) -> Self {
        Self {
            srs_id,
            envelope: Envelope::None,
            empty: false,
            binary_type: BinaryType::Standard,
        }
    }
}

/// header と WKB ペイロードを連結した GPKG geometry blob を生成する。
///
/// ヘッダは LE 固定で書き出す。WKB ペイロードは shpx-geom が LE で生成するが、
/// 本関数はバイト列をそのまま末尾に連結するだけのため、内部 byte order は問わない。
pub fn encode(header: &GpkgBlobHeader, wkb: &[u8]) -> Vec<u8> {
    let env_len = header.envelope.byte_len();
    let mut buf = Vec::with_capacity(8 + env_len + wkb.len());
    buf.extend_from_slice(&MAGIC);
    buf.push(VERSION);
    buf.push(build_flags(header));
    // header endian = LE 固定なので srs_id も LE で書く。
    buf.extend_from_slice(&header.srs_id.to_le_bytes());
    write_envelope_le(header.envelope, &mut buf);
    buf.extend_from_slice(wkb);
    buf
}

/// GPKG geometry blob から header と WKB ペイロードのスライスを取り出す。
///
/// 戻り値の `&[u8]` は引数 `blob` の suffix を借りる。`shpx_geom::wkb::decode`
/// にそのまま渡して `Geom` を復元するのが想定の使い方。
pub fn decode(blob: &[u8]) -> Result<(GpkgBlobHeader, &[u8])> {
    if blob.len() < 8 {
        return Err(Error::Geometry(format!(
            "GPKG blob too short ({} bytes, need >= 8)",
            blob.len()
        )));
    }
    if blob[0..2] != MAGIC {
        return Err(Error::Geometry(format!(
            "GPKG blob magic mismatch: {:#x} {:#x}",
            blob[0], blob[1]
        )));
    }
    if blob[2] != VERSION {
        return Err(Error::Geometry(format!(
            "GPKG blob version unsupported: {}",
            blob[2]
        )));
    }
    let flags = blob[3];
    let little = (flags & FLAG_ENDIAN_LE) != 0;
    let env_type = (flags & FLAG_ENVELOPE_MASK) >> FLAG_ENVELOPE_SHIFT;
    if env_type > 4 {
        return Err(Error::Geometry(format!(
            "GPKG blob: invalid envelope_type {env_type}"
        )));
    }
    let empty = (flags & FLAG_EMPTY) != 0;
    let binary_type = if (flags & FLAG_BINARY_EXTENDED) != 0 {
        BinaryType::Extended
    } else {
        BinaryType::Standard
    };

    // srs_id はヘッダ endian に従う。
    let srs_bytes = [blob[4], blob[5], blob[6], blob[7]];
    let srs_id = if little {
        i32::from_le_bytes(srs_bytes)
    } else {
        i32::from_be_bytes(srs_bytes)
    };

    let env_len = envelope_byte_len(env_type);
    let header_end = 8 + env_len;
    if blob.len() < header_end {
        return Err(Error::Geometry(format!(
            "GPKG blob truncated in envelope (need {header_end} bytes, got {})",
            blob.len()
        )));
    }
    let envelope = read_envelope(&blob[8..header_end], env_type, little)?;

    let header = GpkgBlobHeader {
        srs_id,
        envelope,
        empty,
        binary_type,
    };
    Ok((header, &blob[header_end..]))
}

fn build_flags(header: &GpkgBlobHeader) -> u8 {
    let mut f = FLAG_ENDIAN_LE;
    f |= (header.envelope.type_code() << FLAG_ENVELOPE_SHIFT) & FLAG_ENVELOPE_MASK;
    if header.empty {
        f |= FLAG_EMPTY;
    }
    if matches!(header.binary_type, BinaryType::Extended) {
        f |= FLAG_BINARY_EXTENDED;
    }
    f
}

fn envelope_byte_len(env_type: u8) -> usize {
    // env_type は decode() 側で 0..=4 に検証済み。範囲外を 0 に丸めて未定義動作を避ける。
    match env_type {
        1 => 32,
        2 | 3 => 48,
        4 => 64,
        _ => 0,
    }
}

fn write_envelope_le(env: Envelope, buf: &mut Vec<u8>) {
    let slice: &[f64] = match &env {
        Envelope::None => return,
        Envelope::Xy(a) => a.as_slice(),
        Envelope::Xyz(a) | Envelope::Xym(a) => a.as_slice(),
        Envelope::Xyzm(a) => a.as_slice(),
    };
    for v in slice {
        buf.extend_from_slice(&v.to_le_bytes());
    }
}

fn read_envelope(slice: &[u8], env_type: u8, little: bool) -> Result<Envelope> {
    match env_type {
        0 => Ok(Envelope::None),
        1 => {
            let mut a = [0f64; 4];
            read_f64_array(slice, &mut a, little)?;
            Ok(Envelope::Xy(a))
        }
        2 => {
            let mut a = [0f64; 6];
            read_f64_array(slice, &mut a, little)?;
            Ok(Envelope::Xyz(a))
        }
        3 => {
            let mut a = [0f64; 6];
            read_f64_array(slice, &mut a, little)?;
            Ok(Envelope::Xym(a))
        }
        4 => {
            let mut a = [0f64; 8];
            read_f64_array(slice, &mut a, little)?;
            Ok(Envelope::Xyzm(a))
        }
        _ => unreachable!("envelope_type checked in decode()"),
    }
}

fn read_f64_array(slice: &[u8], out: &mut [f64], little: bool) -> Result<()> {
    if slice.len() < out.len() * 8 {
        return Err(Error::Geometry(format!(
            "GPKG envelope truncated (need {} bytes, got {})",
            out.len() * 8,
            slice.len()
        )));
    }
    for (i, dst) in out.iter_mut().enumerate() {
        let off = i * 8;
        let arr = [
            slice[off],
            slice[off + 1],
            slice[off + 2],
            slice[off + 3],
            slice[off + 4],
            slice[off + 5],
            slice[off + 6],
            slice[off + 7],
        ];
        *dst = if little {
            f64::from_le_bytes(arr)
        } else {
            f64::from_be_bytes(arr)
        };
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wkb;

    #[test]
    fn standard_header_roundtrip_with_point_wkb() {
        let header = GpkgBlobHeader::standard(4326);
        let wkb_bytes = wkb::encode(&wkb::Geom::Point(1.0, 2.0)).unwrap();
        let blob = encode(&header, &wkb_bytes);

        // 先頭 magic + version + flags。
        assert_eq!(blob[0..2], MAGIC);
        assert_eq!(blob[2], VERSION);
        // flags: endian=LE のみ。
        assert_eq!(blob[3], FLAG_ENDIAN_LE);

        let (h, payload) = decode(&blob).unwrap();
        assert_eq!(h, header);
        assert_eq!(payload, wkb_bytes.as_slice());
        let g = wkb::decode(payload).unwrap();
        assert_eq!(g, wkb::Geom::Point(1.0, 2.0));
    }

    #[test]
    fn empty_flag_roundtrip() {
        let header = GpkgBlobHeader {
            srs_id: 0,
            envelope: Envelope::None,
            empty: true,
            binary_type: BinaryType::Standard,
        };
        let blob = encode(&header, &[]);
        let (h, payload) = decode(&blob).unwrap();
        assert!(h.empty);
        assert!(payload.is_empty());
    }

    #[test]
    fn xy_envelope_roundtrip() {
        let env = Envelope::Xy([0.0, 10.0, 0.0, 5.0]);
        let header = GpkgBlobHeader {
            srs_id: 3857,
            envelope: env,
            empty: false,
            binary_type: BinaryType::Standard,
        };
        let blob = encode(&header, b"WKB");
        let (h, payload) = decode(&blob).unwrap();
        assert_eq!(h.envelope, env);
        assert_eq!(payload, b"WKB");
    }

    #[test]
    fn decode_skips_xyzm_envelope() {
        // 手動で XYZM envelope 付きの blob を組み立て、decode が WKB 末尾まで進めることを検証。
        let mut blob = Vec::new();
        blob.extend_from_slice(&MAGIC);
        blob.push(VERSION);
        // flags: LE + envelope_type=4
        blob.push(FLAG_ENDIAN_LE | (4 << FLAG_ENVELOPE_SHIFT));
        blob.extend_from_slice(&(-1_i32).to_le_bytes());
        for v in [0.0_f64, 1.0, 0.0, 1.0, 0.0, 1.0, 0.0, 1.0] {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        blob.extend_from_slice(b"PAYLOAD");

        let (h, payload) = decode(&blob).unwrap();
        assert!(matches!(h.envelope, Envelope::Xyzm(_)));
        assert_eq!(h.srs_id, -1);
        assert_eq!(payload, b"PAYLOAD");
    }

    #[test]
    fn decode_big_endian_header() {
        // ヘッダ BE + envelope_type=0。flags の bit0 (endian) を 0 にする。
        let mut blob = Vec::new();
        blob.extend_from_slice(&MAGIC);
        blob.push(VERSION);
        blob.push(0); // endian=BE, envelope=None, empty=0, standard
        blob.extend_from_slice(&4326_i32.to_be_bytes());
        blob.extend_from_slice(b"WKB");

        let (h, payload) = decode(&blob).unwrap();
        assert_eq!(h.srs_id, 4326);
        assert_eq!(h.envelope, Envelope::None);
        assert_eq!(payload, b"WKB");
    }

    #[test]
    fn decode_extended_binary_type_flag() {
        let header = GpkgBlobHeader {
            srs_id: 0,
            envelope: Envelope::None,
            empty: false,
            binary_type: BinaryType::Extended,
        };
        let blob = encode(&header, &[]);
        let (h, _) = decode(&blob).unwrap();
        assert_eq!(h.binary_type, BinaryType::Extended);
    }

    #[test]
    fn rejects_bad_magic() {
        let blob = [0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        let err = decode(&blob).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("magic")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_unsupported_version() {
        let mut blob = vec![MAGIC[0], MAGIC[1]];
        blob.push(1);
        blob.push(FLAG_ENDIAN_LE);
        blob.extend_from_slice(&0_i32.to_le_bytes());
        let err = decode(&blob).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("version")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_truncated_envelope() {
        let mut blob = vec![MAGIC[0], MAGIC[1]];
        blob.push(VERSION);
        // envelope_type=1 (32 bytes 必要) を宣言しつつ envelope 部を削る。
        blob.push(FLAG_ENDIAN_LE | (1 << FLAG_ENVELOPE_SHIFT));
        blob.extend_from_slice(&0_i32.to_le_bytes());
        // envelope 用の 32 bytes は付けない。
        let err = decode(&blob).unwrap_err();
        match err {
            Error::Geometry(msg) => assert!(msg.contains("envelope") || msg.contains("truncated")),
            other => panic!("expected Error::Geometry, got {other:?}"),
        }
    }

    #[test]
    fn rejects_too_short_blob() {
        let blob = [0x47];
        let err = decode(&blob).unwrap_err();
        assert!(matches!(err, Error::Geometry(_)));
    }
}
