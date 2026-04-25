//! Driver の能力宣言。コアパイプラインがこの値に応じて bulk path 選択や損失警告を行う。

/// 1 つの Driver が「何ができるか」を宣言する構造体。
//
// 各能力フラグは独立に意味を持つため、enum 化はせず bool で表現する方針を取る。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// 読み出し可能。
    pub read: bool,
    /// 書き込み可能。
    pub write: bool,
    /// ランダムアクセス（オフセット指定読み出し）対応。
    pub random_access: bool,
    /// `BulkLoadWriter` の最適化パスを持つ（PostgreSQL COPY、SQL Server bulk_insert 等）。
    pub bulk_load: bool,
    /// バイナリ列（Arrow `Binary`/`LargeBinary`）の格納をサポートする。
    pub supports_blob: bool,
    /// Arrow `Decimal128` / `Decimal256` の精度保全をサポートする。
    pub supports_decimal: bool,
    /// `Timestamp(_, Some(tz))` のタイムゾーン付き保存をサポートする。
    pub supports_timestamp_tz: bool,
    /// 文字列カラムのエンコーディング。出力時にどの codec を許すかを宣言する。
    pub string_encoding: StringEncoding,
    /// decimal の最大精度（桁数 `p`）。`None` は無制限を意味する。
    pub max_decimal_precision: Option<u8>,
}

/// 文字列カラムのエンコーディング指定。
#[derive(Debug, Clone)]
pub enum StringEncoding {
    /// 単一の固定エンコーディングしか許さない（例: GeoParquet は UTF-8 固定）。
    Fixed(&'static str),
    /// 複数のエンコーディングを許す（例: Shapefile は cpg で codec 切替可能）。
    /// 第 1 要素が既定値として扱われる。
    Configurable(&'static [&'static str]),
}

impl Capabilities {
    /// 「読み専用・最低限」の Capabilities を返す。テストや stub 用途。
    pub const fn read_only_minimal() -> Self {
        Self {
            read: true,
            write: false,
            random_access: false,
            bulk_load: false,
            supports_blob: false,
            supports_decimal: false,
            supports_timestamp_tz: false,
            string_encoding: StringEncoding::Fixed("utf-8"),
            max_decimal_precision: None,
        }
    }
}
