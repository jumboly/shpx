//! CLI のサブコマンド定義（clap derive）。

use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};
use shpx_core::OnLoss;

#[derive(Parser, Debug)]
#[command(
    name = "shpx",
    version,
    about = "ジオ空間データ変換 CLI（v0.1: Shapefile / GeoParquet）"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Cmd,

    /// `-v` で INFO、`-vv` で DEBUG、`-vvv` で TRACE。`RUST_LOG` 環境変数も尊重。
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// 入力ファイルを読み、別フォーマットで出力する。
    Convert(ConvertArgs),
    /// 入力ファイルのスキーマ・CRS・行数を表示する。
    Info(InfoArgs),
    /// 入力ファイルの Arrow スキーマを JSON で出力する。
    Schema(SchemaArgs),
    /// 登録されている driver と各 capabilities を一覧表示する。
    Drivers,
}

#[derive(clap::Args, Debug)]
pub struct ConvertArgs {
    /// 入力ファイル（拡張子から driver を推論）。
    pub src: PathBuf,
    /// 出力ファイル（拡張子から driver を推論）。
    pub dst: PathBuf,

    /// 既存出力ファイルを上書きする。
    #[arg(long)]
    pub overwrite: bool,

    /// 損失変換ポリシー。既定 `error` は安全側で中断する。
    #[arg(long, value_enum, default_value_t = OnLossArg::Error)]
    pub on_loss: OnLossArg,

    /// 入出力エンコーディング（Shapefile などで `.cpg` ファイルへ反映）。
    #[arg(long)]
    pub encoding: Option<String>,

    /// 書き込み時の希望 batch サイズ。Driver は無視する場合がある。
    #[arg(long)]
    pub batch_size: Option<usize>,

    /// 入力 CRS が無いとき補完する EPSG（例: `EPSG:4326`）。
    #[arg(long)]
    pub src_crs: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct InfoArgs {
    /// 入力ファイル。
    pub src: PathBuf,

    /// 入力 CRS が無いとき補完する EPSG。
    #[arg(long)]
    pub src_crs: Option<String>,

    /// 入力エンコーディング（`.cpg` 不在の Shapefile 等で有用）。
    #[arg(long)]
    pub encoding: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct SchemaArgs {
    /// 入力ファイル。
    pub src: PathBuf,

    /// 入力 CRS が無いとき補完する EPSG。
    #[arg(long)]
    pub src_crs: Option<String>,

    /// 入力エンコーディング（`.cpg` 不在の Shapefile 等で有用）。
    #[arg(long)]
    pub encoding: Option<String>,

    /// 出力 JSON を pretty-print する。既定は 1 行 compact 出力。
    #[arg(long)]
    pub pretty: bool,
}

/// CLI 表面の `--on-loss`。`shpx_core::OnLoss` への 1:1 マッピング。
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum OnLossArg {
    Error,
    Warn,
    Skip,
}

impl From<OnLossArg> for OnLoss {
    fn from(v: OnLossArg) -> Self {
        match v {
            OnLossArg::Error => OnLoss::Error,
            OnLossArg::Warn => OnLoss::Warn,
            OnLossArg::Skip => OnLoss::Skip,
        }
    }
}
