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
}
