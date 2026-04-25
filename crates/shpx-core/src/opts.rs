//! 読み書き時のオプション構造体。
//!
//! CLI が解析した値をここに詰めて Driver に渡す。Driver 固有のオプションは
//! 将来 `extra: BTreeMap<String, String>` 等で拡張する余地を残す。

use crate::Crs;

/// 損失変換時の挙動。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnLoss {
    /// 既定。損失を検知したら即座に [`crate::Error::OnLoss`] で中断する。
    #[default]
    Error,
    /// 警告ログを出しつつ可能な範囲で降格させる（例: decimal を double へ）。
    Warn,
    /// 該当フィールドを無音でスキップする。
    Skip,
}

/// Reader 用のオプション。
#[derive(Debug, Clone, Default)]
pub struct ReadOpts {
    /// 入力に CRS が無い場合に補完する EPSG（CLI の `--src-crs`）。
    pub src_crs: Option<Crs>,
    /// 入力時のエンコーディング指定（cpg ファイル不在の Shapefile などで有用）。
    pub encoding: Option<String>,
    /// CLI の `--where '<sql>'`。RDB driver（PostGIS など）が `WHERE` 句として埋め込む。
    /// ファイル driver は無視する。
    pub where_clause: Option<String>,
    /// CLI の `--select col1,col2,...`。RDB driver が投影列を絞り込むのに使う。
    /// ファイル driver は無視する。`Some(vec![])` は CLI 段階で除外され `None` に正規化される。
    pub select: Option<Vec<String>>,
    /// CLI の `--query 'SELECT ...'`。RDB driver が任意 SQL をサブクエリ化して読み出す。
    /// `--where` / `--select` とは排他（CLI 側で clap の `conflicts_with_all` で弾く）。
    pub query: Option<String>,
}

/// Writer 用のオプション。
#[derive(Debug, Clone, Default)]
pub struct WriteOpts {
    /// 出力時のエンコーディング指定（Shapefile では cpg ファイルへ反映）。
    pub encoding: Option<String>,
    /// 損失変換ポリシー。
    pub on_loss: OnLoss,
    /// 既存ファイルを上書きする。
    pub overwrite: bool,
    /// 1 RecordBatch あたりの行数の希望値。Driver がこの値を尊重する保証は無い。
    pub batch_size_hint: Option<usize>,
    /// テーブル作成戦略（RDB driver でのみ意味を持つ）。ファイル driver は無視する。
    pub create_table: CreateTable,
    /// 空間インデックス自動生成戦略（RDB driver でのみ意味を持つ）。ファイル driver は無視する。
    pub create_index: CreateIndex,
}

/// テーブル作成戦略。RDB driver (PostGIS など) でのみ意味を持つ。
///
/// `--overwrite` と組み合わせる際の規約:
/// - `--overwrite=true` && `Never` は driver 側で整合性エラーにする（DROP した直後に
///   テーブルが無い状態で INSERT は不可能なため）。
/// - `--overwrite=true` は事実上 `Always` 相当の挙動を要求するが、CLI の互換性のため
///   独立フラグとして共存させる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreateTable {
    /// 既定。テーブルが存在しなければ作成、あれば触らずに既存スキーマへ書き込む（append）。
    #[default]
    IfNotExists,
    /// 既存有無に関わらず CREATE を発行する。`--overwrite=true` と併用すれば DROP→CREATE。
    /// 既存テーブルがあれば PG エラー (relation already exists) で停止する。
    Always,
    /// CREATE を一切発行しない。事前に手動で作成済みの既存テーブルへ append する用途。
    /// テーブルが存在しなければ driver 側で `Error::Driver` を返す。
    Never,
}

/// 空間インデックス自動生成戦略。RDB driver でのみ意味を持つ。
///
/// PostGIS では bulk load 後に GIST index を作るのが定石（COPY 前に index があると
/// 1 桁遅くなる）。本オプションは `LayerWriter::finish` / `BulkLoadWriter::bulk_write`
/// の最後に `CREATE INDEX IF NOT EXISTS ... USING GIST (geom_col)` を発行する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreateIndex {
    /// 既定。writer がテーブルを **新規作成した場合のみ** index を発行する。`Never` 経路、
    /// および `IfNotExists` 経路で既存テーブルへ append したケースでは触らない。
    #[default]
    Auto,
    /// `create_table` の値に関わらず必ず index を発行する (`IF NOT EXISTS` で重複は安全)。
    Always,
    /// index を一切発行しない。
    Never,
}
