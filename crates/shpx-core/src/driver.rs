//! Driver / Layer 読み書きのコアトレイト群。
//!
//! 各フォーマット実装はこれらを実装し、CLI は `Driver` 配列に対して
//! 拡張子・スキームから 1 つを選んで読み書きする。
//!
//! Driver の登録は [`inventory`] crate を経由して行う。各 driver crate は
//! [`DriverRegistration`] を `inventory::submit!` で送り、CLI 側は
//! `inventory::iter::<DriverRegistration>` で起動時に集約する。

use arrow_array::RecordBatch;
use arrow_schema::SchemaRef;

use crate::{Capabilities, Crs, ReadOpts, Result, Uri, WriteOpts};

/// 1 つの読み書き戦略を表すドライバ。
///
/// Driver は Format（オンディスク/ワイヤ形式）と 1:1 ではない — 同一 Format に
/// 複数 Driver が存在しうる（例: SQLite に対し gpkg / spatialite）。どの Driver で
/// 解決するかは内容判別ではなく Scheme（拡張子・URL scheme）で決まる。
/// 用語の境界は repo ルートの `CONTEXT.md` 参照。
///
/// 「ドライバ」はステートレスな factory として実装することを推奨する。
/// 実際の I/O 状態は [`LayerReader`] / [`LayerWriter`] が保持する。
pub trait Driver: Send + Sync + 'static {
    /// 識別名（小文字、ハイフン無し）。例: `"shp"`、`"parquet"`。
    fn name(&self) -> &'static str;

    /// 担当する URI スキーム / 拡張子の一覧（小文字）。
    /// CLI の拡張子推論はここを照合する。
    fn supported_schemes(&self) -> &[&'static str];

    /// このドライバが何をサポートするかを宣言する。
    fn capabilities(&self) -> Capabilities;

    /// 入力レイヤを開く。
    fn open_read(&self, uri: &Uri, opts: &ReadOpts) -> Result<Box<dyn LayerReader>>;

    /// 出力レイヤを開く。
    ///
    /// `schema` は Reader から伝搬したスキーマで、`crs` は明示指定または
    /// Reader から取得した CRS。Driver はこれを基に出力先のヘッダ・メタデータを書く。
    fn open_write(
        &self,
        uri: &Uri,
        schema: SchemaRef,
        crs: Option<Crs>,
        opts: &WriteOpts,
    ) -> Result<Box<dyn LayerWriter>>;

    /// バルクロード経路で出力レイヤを開く。`BulkLoadWriter` を実装する Driver のみ
    /// `Some(...)` を返す。default 実装は `Ok(None)` で、CLI 側は本メソッドの戻り値が
    /// `None` なら通常の [`open_write`](Self::open_write) 経路にフォールバックする。
    ///
    /// 引数は [`open_write`](Self::open_write) と同一。Driver 内部では同じ初期化処理を
    /// 通すことを想定している（戻り値の dyn 型だけが違う）。
    fn open_bulk_write(
        &self,
        _uri: &Uri,
        _schema: SchemaRef,
        _crs: Option<Crs>,
        _opts: &WriteOpts,
    ) -> Result<Option<Box<dyn BulkLoadWriter>>> {
        Ok(None)
    }
}

/// レイヤ読み出し: 先頭でスキーマを確定し、その後バッチを順に列挙する。
pub trait LayerReader: Send {
    /// 読み出しスキーマ。`batches()` を呼び出す前から確定している。
    fn schema(&self) -> SchemaRef;

    /// 既知の CRS。`None` は CRS 情報が無いことを意味する。
    fn crs(&self) -> Option<&Crs>;

    /// 全レコード数のヒント。巨大ファイルや streaming source では `None` で良い。
    fn row_count_hint(&self) -> Option<usize>;

    /// バッチを順次列挙する。Iterator は `&mut self` のライフタイムに紐付く。
    fn batches(&mut self) -> Box<dyn Iterator<Item = Result<RecordBatch>> + Send + '_>;
}

/// レイヤ書き出し: バッチを push し、最後に [`finish`] で finalize する。
///
/// [`finish`]: LayerWriter::finish
pub trait LayerWriter: Send {
    /// 1 バッチ書き込む。複数回呼ぶことができる。
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<()>;

    /// 書き終わり処理。フッタや index、ファイルクローズを行う。
    /// `Box<Self>` を消費するため呼び出し後はオブジェクトを使えない。
    fn finish(self: Box<Self>) -> Result<()>;
}

/// バルクロードの最適化パスを持つ Writer 用拡張トレイト。
///
/// PostgreSQL `COPY BINARY` や SQL Server `bulk_insert` 等、バッチ単位 INSERT より
/// 高速なパスを持つ Driver で実装する。v0.1 では使用されない（PostgreSQL 等は v0.3 以降）。
pub trait BulkLoadWriter: LayerWriter {
    /// イテレータ全体を 1 つのバルク投入として処理する。
    fn bulk_write(&mut self, batches: &mut dyn Iterator<Item = Result<RecordBatch>>) -> Result<()>;
}

/// `inventory` 経由で登録される Driver エントリ。
///
/// `&'static dyn Driver` を保持することで、Driver 本体を `static` に置いたまま
/// アロケーション無しでレジストリへ登録できる。各 driver crate は次のように使う:
///
/// ```ignore
/// static MY_DRIVER: MyDriver = MyDriver;
/// shpx_core::inventory::submit! {
///     shpx_core::DriverRegistration { driver: &MY_DRIVER }
/// }
/// ```
// `Driver` トレイトが `Debug` を要求しないため、`DriverRegistration` も
// `Debug` を derive しない（`#[derive(Debug)]` は trait object に伝播してしまう）。
#[derive(Clone, Copy)]
pub struct DriverRegistration {
    /// 登録対象の Driver。`'static` 寿命なので `&` のままで安全に使える。
    pub driver: &'static dyn Driver,
}

inventory::collect!(DriverRegistration);
