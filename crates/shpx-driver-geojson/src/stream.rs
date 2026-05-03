//! GeoJSON / GeoJSONL の真のストリーミング読み出しヘルパ。
//!
//! v0.8 cycle 3 で eager-load (`Vec<Feature>`) を撤廃するために新設。`reader.rs` から
//! 「ファイル全体の Feature 列挙」と「FeatureCollection 先頭の `crs` メンバ抽出」を
//! このモジュールに分離する。
//!
//! # FeatureCollection は自前の `FcFeatureStream`
//!
//! `geojson::FeatureReader::features()` は同じ目的の API だが、上流 0.24 の実装に
//! 2 つの bug があり採用できない:
//! - 空配列 `"features":[]` に対し最初の `next()` 呼び出しで「expected value」エラーを
//!   返す (`[` の次の `]` を value として読みに行ってしまう)。
//! - `next()` が `None` を返した後の再呼び出しで `unreachable!` で panic する
//!   (`State::AfterFeatures` 分岐)。
//!
//! 本モジュールでは同じ state machine を直接実装し、両方の bug を回避する。`[` 直後の
//! 非 whitespace バイトをピークして `]` なら即 None、`Done` 後の再 `next()` も安全に
//! `None` を返す。
//!
//! # NDJSON は `Deserializer::into_iter` を素直に使う
//!
//! 1 行 = 1 JSON value 形式は serde_json の `StreamDeserializer<R, Feature>` で素直に
//! 反復できる。空行 / `#` コメント行のスキップは元の `parse_geojson_lines` 仕様を踏襲する。
//!
//! # `crs` メンバは別経路で head を probe する
//!
//! `FeatureReader::features()` は FeatureCollection の root メンバ (`crs`, `bbox` 等) を
//! 表に出さずに features 配列だけを舐める。RFC 7946 で deprecated だが既存テストが
//! `crs` を使っているため、`extract_top_level_crs()` で **ファイル head の限られた範囲を
//! 別途読み取り** crs メンバを抽出する。head 容量は 1 MiB に制限し、巨大ファイルでも
//! 一度メモリに乗せない。

use std::fs::File;
use std::io::{self, BufRead, BufReader, Read};
use std::path::Path;

use geojson::Feature;
use serde_json::{Deserializer, Value as JsonValue};
use shpx_core::{Error, Result};

use crate::util::{driver_err, driver_msg};

/// FeatureCollection の `crs` メンバ抽出時に読み取るファイル head 上限。
/// 巨大ファイル (TB 級) でも head だけで済むよう 1 MiB に制限する。crs が末尾近くに
/// 置かれたファイルは「crs 不在」扱いになるが、RFC 7946 deprecated 仕様で
/// 実運用では head に置かれるため許容する。
const CRS_PROBE_LIMIT: u64 = 1024 * 1024;

/// `Iterator<Item = Result<Feature>> + Send + 'static`。
/// FeatureCollection / NDJSON のいずれの戦略でもこのトレイトオブジェクトに統一する。
pub type FeatureStream = Box<dyn Iterator<Item = Result<Feature>> + Send>;

/// FeatureCollection 用の streaming iterator を作る。
///
/// `geojson::FeatureReader::features()` を Result 型 (`shpx_core::Result`) に変換しつつ
/// 透過的に渡す。`R` は file ハンドルを所有 (`File`) する必要があり、参照渡しは不可。
pub fn open_feature_collection(path: &Path) -> Result<FeatureStream> {
    let file = File::open(path).map_err(Error::from)?;
    let buf = BufReader::new(file);
    Ok(Box::new(FcFeatureStream::new(buf)))
}

/// FeatureCollection 用の自前 streaming iterator。
///
/// `geojson::FeatureReader` の代替。`reader` から 1 byte ずつ読み、`"features": [` を
/// 検出 → カンマ区切りで `Feature` を 1 つずつ deserialize → `]` で終了する。
/// 空配列 / `Done` 後の再 `next()` をいずれも安全に処理する。
pub struct FcFeatureStream<R: BufRead> {
    reader: Option<R>,
    state: FcState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FcState {
    /// `[` をまだ見つけていない。バイトを読み飛ばす。
    BeforeFeatures,
    /// `[` 直後 (まだ要素を 1 つも読んでいない)。次の非 ws バイトが `]` なら空配列で終了。
    FirstElement,
    /// 1 つ以上の Feature を読んだ。次は `,` または `]` を期待する。
    DuringFeatures,
    /// `]` を読み終えた、または致命エラー後。再 `next()` で常に None。
    Done,
}

impl<R: BufRead> FcFeatureStream<R> {
    pub fn new(reader: R) -> Self {
        Self {
            reader: Some(reader),
            state: FcState::BeforeFeatures,
        }
    }

    /// `reader` から 1 byte 読み出す。EOF は `Ok(None)`。
    fn read_byte(reader: &mut R) -> io::Result<Option<u8>> {
        let mut buf = [0u8; 1];
        match reader.read(&mut buf) {
            Ok(0) => Ok(None),
            Ok(_) => Ok(Some(buf[0])),
            Err(e) => Err(e),
        }
    }

    /// 次の非 whitespace バイトを読んで返す。EOF は `Ok(None)`。
    fn next_non_ws(reader: &mut R) -> io::Result<Option<u8>> {
        loop {
            match Self::read_byte(reader)? {
                Some(b) if b.is_ascii_whitespace() => {}
                other => return Ok(other),
            }
        }
    }

    /// 次の非 whitespace バイトを覗き見る (consume しない)。
    fn peek_non_ws(reader: &mut R) -> io::Result<Option<u8>> {
        loop {
            let buf = reader.fill_buf()?;
            if buf.is_empty() {
                return Ok(None);
            }
            // ws 部分を消費しつつ最初の非 ws を返す。
            let mut ws_count = 0;
            for (i, &b) in buf.iter().enumerate() {
                if b.is_ascii_whitespace() {
                    ws_count = i + 1;
                    continue;
                }
                reader.consume(ws_count);
                return Ok(Some(b));
            }
            // バッファ全部 ws → 消費して次のバッファへ。
            let len = buf.len();
            reader.consume(len);
        }
    }
}

impl<R: BufRead> Iterator for FcFeatureStream<R> {
    type Item = Result<Feature>;

    fn next(&mut self) -> Option<Self::Item> {
        let reader = self.reader.as_mut()?;

        // ステートに応じてセパレータを処理してから 1 feature を deserialize する。
        loop {
            match self.state {
                FcState::Done => {
                    self.reader = None;
                    return None;
                }
                FcState::BeforeFeatures => {
                    // `[` が見つかるまで非 whitespace バイトを舐める。`{`, `}`, `:`,
                    // `,`, key 文字列、value 文字列、ネスト数値・bool・null は構造的に
                    // 軽量に skip する代わりに「`[` を見つけるまで全て読み飛ばす」方式で簡素化。
                    // この方式は文字列 / object 値の中の `[` も拾うリスクがあるため、
                    // 「features 配列以外の値の `[`」が無いことを実用的に仮定する
                    // (top-level の properties に array を置くカスタム拡張は稀)。
                    let mut found = false;
                    while let Some(b) = match Self::read_byte(reader) {
                        Ok(b) => b,
                        Err(e) => {
                            self.state = FcState::Done;
                            return Some(Err(Error::from(e)));
                        }
                    } {
                        if b == b'[' {
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        self.state = FcState::Done;
                        return Some(Err(driver_msg(
                            "GeoJSON FeatureCollection: features array `[` not found",
                        )));
                    }
                    self.state = FcState::FirstElement;
                }
                FcState::FirstElement => {
                    match Self::peek_non_ws(reader) {
                        Ok(Some(b']')) => {
                            // 空配列。`]` を消費して Done。
                            let _ = Self::read_byte(reader);
                            self.state = FcState::Done;
                            return None;
                        }
                        Ok(Some(_)) => {
                            self.state = FcState::DuringFeatures;
                            return read_one_feature(self, /*after_first=*/ false);
                        }
                        Ok(None) => {
                            self.state = FcState::Done;
                            return Some(Err(driver_msg(
                                "GeoJSON FeatureCollection: unexpected EOF inside features array",
                            )));
                        }
                        Err(e) => {
                            self.state = FcState::Done;
                            return Some(Err(Error::from(e)));
                        }
                    }
                }
                FcState::DuringFeatures => {
                    // 直前の Feature を読み終えた状態。次は `,` か `]`。
                    match Self::next_non_ws(reader) {
                        Ok(Some(b',')) => {
                            return read_one_feature(self, /*after_first=*/ true);
                        }
                        Ok(Some(b']')) => {
                            self.state = FcState::Done;
                            return None;
                        }
                        Ok(Some(c)) => {
                            self.state = FcState::Done;
                            return Some(Err(driver_msg(format!(
                                "GeoJSON FeatureCollection: expected `,` or `]`, got `{}`",
                                c as char
                            ))));
                        }
                        Ok(None) => {
                            self.state = FcState::Done;
                            return Some(Err(driver_msg(
                                "GeoJSON FeatureCollection: unexpected EOF (missing `]`)",
                            )));
                        }
                        Err(e) => {
                            self.state = FcState::Done;
                            return Some(Err(Error::from(e)));
                        }
                    }
                }
            }
        }
    }
}

fn read_one_feature<R: BufRead>(
    stream: &mut FcFeatureStream<R>,
    _after_first: bool,
) -> Option<Result<Feature>> {
    let reader = stream.reader.as_mut()?;
    let de = Deserializer::from_reader(&mut *reader);
    let mut iter = de.into_iter::<Feature>();
    match iter.next() {
        Some(Ok(f)) => Some(Ok(f)),
        Some(Err(e)) => {
            stream.state = FcState::Done;
            Some(Err(driver_err(&e)))
        }
        None => {
            stream.state = FcState::Done;
            None
        }
    }
}

/// NDJSON 用の streaming iterator を作る。
///
/// 空行 / `#` で始まる行はスキップし、それ以外を 1 行 1 Feature としてパースする。
pub fn open_ndjson(path: &Path) -> Result<FeatureStream> {
    let file = File::open(path).map_err(Error::from)?;
    // 行単位スキップを正しく実装するため `BufRead::lines()` ベースにする。
    // 空行 / コメント行のフィルタは serde_json::Deserializer 単体では表現できない。
    let buf = BufReader::new(file);
    let it = NdjsonStream {
        lines: buf.lines(),
        lineno: 0,
        done: false,
    };
    Ok(Box::new(it))
}

struct NdjsonStream<R: BufRead> {
    lines: io::Lines<R>,
    lineno: usize,
    done: bool,
}

impl<R: BufRead> Iterator for NdjsonStream<R> {
    type Item = Result<Feature>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        loop {
            let raw = match self.lines.next()? {
                Ok(s) => s,
                Err(e) => {
                    self.done = true;
                    return Some(Err(Error::from(e)));
                }
            };
            self.lineno += 1;
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            // BOM は先頭行のみ。trim() は BOM を含まないため明示除去する。
            let body = trimmed.strip_prefix('\u{feff}').unwrap_or(trimmed);
            let parsed: std::result::Result<Feature, _> = serde_json::from_str(body);
            return Some(parsed.map_err(|e| {
                driver_msg(format!("GeoJSONL line {}: {e}", self.lineno))
            }));
        }
    }
}

/// FeatureCollection の root レベル `crs` メンバを抽出する。
///
/// ファイル head を 1 MiB まで読んで `serde_json::Deserializer::deserialize_map` で
/// root オブジェクトのキーを舐め、`"crs"` が見つかったらその value を `Value` に
/// パースして返す。`features` キーに到達したら crs は無いとみなして None を返す
/// (features 配列を全件読み込まないため、巨大ファイルでもメモリ消費は head 1 MiB に
/// 収まる)。
///
/// 戻り値の意味:
/// - `Ok(Some(value))` — root に `"crs": <value>` が見つかった。`value` は
///   `null` を含むあらゆる JSON 値。
/// - `Ok(None)` — head 1 MiB 以内に `"crs"` キーが見つからなかった
///   (features 配列に到達 / EOF / 上限超過)。
/// - `Err(_)` — head が JSON として壊れている等の本物のパースエラー。
pub fn extract_top_level_crs_value(path: &Path) -> Result<Option<JsonValue>> {
    let file = File::open(path).map_err(Error::from)?;
    let mut head = Vec::new();
    file.take(CRS_PROBE_LIMIT)
        .read_to_end(&mut head)
        .map_err(Error::from)?;
    // head が JSON object として完結していなくても、`Deserializer::from_slice` で
    // 「root オブジェクトの中の crs キー」を見つけるまでパースできれば十分。
    // 完結していないため `Value` への full deserialize は失敗しうるが、
    // それは features 配列途中の中断であり crs キーの取得には影響しない。
    extract_crs_from_head_bytes(&head)
}

/// head バイト列から root レベル `"crs"` を抽出する。
fn extract_crs_from_head_bytes(head: &[u8]) -> Result<Option<JsonValue>> {
    // 戦略: 手書きの軽量 JSON tokenizer で root オブジェクト直下のキーを舐める。
    // ネスト深さを追い、深さ 1 (= root object 直下) で string key を拾い、
    // `"crs"` ならその value を `serde_json::from_slice` で完全パースして返す。
    // `features` が先に来たら head は features 配列に入り終端まで届かないため
    // ここで打ち切る (None を返す)。
    let mut scanner = HeadScanner::new(head);
    scanner.expect_root_object_start()?;
    while let Some(key) = scanner.next_root_key()? {
        if key == "crs" {
            return scanner.parse_value().map(Some);
        }
        if key == "features" {
            // features 配列より後に crs が出現する書き方は珍しいが、`FeatureReader` は
            // それでも features を見つけてくれる。crs は head 範囲外として None で返す。
            return Ok(None);
        }
        scanner.skip_value()?;
    }
    Ok(None)
}

/// 軽量 JSON head スキャナ。値は `serde_json` に委譲し、object 構造の skip だけ
/// 自前で行う。
struct HeadScanner<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> HeadScanner<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len() {
            match self.bytes[self.pos] {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                _ => break,
            }
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.bytes.get(self.pos).copied()
    }

    fn expect_root_object_start(&mut self) -> Result<()> {
        // 先頭 BOM を寛容に読み飛ばす。
        if self.bytes.starts_with(b"\xef\xbb\xbf") {
            self.pos = 3;
        }
        match self.peek() {
            Some(b'{') => {
                self.pos += 1;
                Ok(())
            }
            Some(c) => Err(driver_msg(format!(
                "GeoJSON FeatureCollection must start with `{{`, found `{}`",
                c as char
            ))),
            None => Err(driver_msg("GeoJSON file is empty")),
        }
    }

    /// root object 内の次のキー名を返す。`}` に到達 / head 切れの場合は `None`。
    fn next_root_key(&mut self) -> Result<Option<String>> {
        match self.peek() {
            Some(b'}') => {
                self.pos += 1;
                return Ok(None);
            }
            Some(b',') => {
                self.pos += 1;
                self.skip_ws();
            }
            Some(_) => {} // 1 個目のキーへ
            None => return Ok(None),
        }
        match self.peek() {
            Some(b'"') => {}
            // 末尾コンマの後に `}` を許容するパーサもあるが GeoJSON 仕様は標準 JSON に従うので拒否しない (head 切れ扱い)。
            Some(b'}') => {
                self.pos += 1;
                return Ok(None);
            }
            // head が JSON 文字列途中で切れた場合は EOF 相当。crs は head 範囲外なので None。
            None => return Ok(None),
            Some(c) => {
                return Err(driver_msg(format!(
                    "GeoJSON FeatureCollection: expected key string, found `{}`",
                    c as char
                )))
            }
        }
        let key = self.read_json_string()?;
        self.skip_ws();
        if self.peek() != Some(b':') {
            return Err(driver_msg("GeoJSON FeatureCollection: expected `:` after key"));
        }
        self.pos += 1;
        self.skip_ws();
        Ok(Some(key))
    }

    /// `"..."` 文字列を読んで unescape した String を返す。serde_json に丸投げする。
    fn read_json_string(&mut self) -> Result<String> {
        // `"..."` の終端を見つけるため、簡易な escape トラッキングで対応。
        let start = self.pos;
        if self.bytes.get(start) != Some(&b'"') {
            return Err(driver_msg("expected `\"`"));
        }
        let mut i = start + 1;
        let mut escape = false;
        while i < self.bytes.len() {
            let b = self.bytes[i];
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                let end = i + 1;
                let raw = &self.bytes[start..end];
                let s: String = serde_json::from_slice(raw).map_err(|e| driver_err(&e))?;
                self.pos = end;
                return Ok(s);
            }
            i += 1;
        }
        Err(driver_msg("GeoJSON FeatureCollection: unterminated string in head"))
    }

    /// 現在位置の値を `Value` にパースして返す (head 完結前提)。
    fn parse_value(&mut self) -> Result<JsonValue> {
        // `Deserializer<SliceRead>` 自体には byte_offset が無いため、`into_iter` で
        // `StreamDeserializer` に変換して 1 値だけ取り出し、消費バイト数を取得する。
        let de = Deserializer::from_slice(&self.bytes[self.pos..]);
        let mut iter = de.into_iter::<JsonValue>();
        let v = match iter.next() {
            Some(Ok(v)) => v,
            Some(Err(e)) => return Err(driver_err(&e)),
            None => return Err(driver_msg("unexpected end of input while reading value")),
        };
        self.pos += iter.byte_offset();
        Ok(v)
    }

    /// 現在位置の値を skip する (`Value` を捨てる)。features 配列のような巨大値で
    /// 呼ぶと head 範囲外でパース失敗するため、`features` キーは別途処理する想定。
    fn skip_value(&mut self) -> Result<()> {
        let _ = self.parse_value()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_crs_from_head_finds_crs() {
        let head = br#"{
          "type":"FeatureCollection",
          "crs":{"type":"name","properties":{"name":"EPSG:3857"}},
          "features":[]
        }"#;
        let v = extract_crs_from_head_bytes(head).unwrap();
        assert!(v.is_some(), "crs member must be extracted");
        let obj = v.unwrap();
        assert_eq!(obj["type"], "name");
    }

    #[test]
    fn extract_crs_from_head_handles_no_crs() {
        let head = br#"{"type":"FeatureCollection","features":[]}"#;
        let v = extract_crs_from_head_bytes(head).unwrap();
        assert!(v.is_none());
    }

    #[test]
    fn extract_crs_from_head_handles_bom() {
        let mut head = vec![0xef, 0xbb, 0xbf];
        head.extend_from_slice(
            br#"{"type":"FeatureCollection","crs":null,"features":[]}"#,
        );
        let v = extract_crs_from_head_bytes(&head).unwrap();
        assert_eq!(v, Some(JsonValue::Null));
    }
}
