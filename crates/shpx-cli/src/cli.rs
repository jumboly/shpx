//! CLI のサブコマンド定義（clap derive）。

use clap::{ArgAction, Parser, Subcommand};
use shpx_core::OnLoss;

#[derive(Parser, Debug)]
#[command(
    name = "shpx",
    version,
    about = "ジオ空間データ変換 CLI（v0.3: SHP / GeoParquet / CSV / GeoJSON / GPKG / FlatGeobuf / PostGIS + --reproject）"
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

// `src` / `dst` は PathBuf ではなく String で受ける。`pg://user:pass@host/db?table=t`
// のような URL 入力は OS パスとして解釈されると壊れるため（特に Windows のドライブ
// レター扱いになりうる）、文字列のまま `Uri::from_path` に渡す。
#[derive(clap::Args, Debug)]
pub struct ConvertArgs {
    /// 入力ファイルパス または URL（`pg://...` 等）。
    pub src: String,
    /// 出力ファイルパス または URL（`pg://...` 等）。
    pub dst: String,

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

    /// 出力時に reproject する目標 CRS（例: `EPSG:3857`）。WKT2 / proj-string も受理する。
    /// 入力 CRS が解決できないとエラーになるため `--src-crs` と併用すること。
    #[arg(long)]
    pub reproject: Option<String>,

    /// 出力 driver の bulk 経路を使うかどうか。`auto` (既定) は `Capabilities::bulk_load`
    /// が真の driver で bulk 経路、それ以外は batch 経路。`bulk` 明示時は非対応 driver で
    /// エラー。`batch` 明示時は常に行単位経路。PostGIS は cycle 2 で bulk 経路 (COPY BINARY)
    /// 対応のため、既定で COPY BINARY が使われる。
    #[arg(long, value_enum, default_value_t = InsertModeArg::Auto)]
    pub insert_mode: InsertModeArg,

    /// 入力テーブルへの `WHERE` 条件式（PostGIS など RDB driver でのみ有効）。
    /// 例: `--where "id < 100 AND status = 'active'"`。`--query` とは排他。
    #[arg(long = "where", value_name = "SQL")]
    pub where_clause: Option<String>,

    /// 投影する列名のカンマ区切りリスト。geometry 列は必ず含めること
    /// （PostGIS など RDB driver でのみ有効）。例: `--select id,name,geom`。`--query` とは排他。
    #[arg(long, value_name = "COL[,COL...]", value_delimiter = ',')]
    pub select: Vec<String>,

    /// 任意の `SELECT` 文をサブクエリ化して読み出す（PostGIS など RDB driver でのみ有効）。
    /// `--where` / `--select` とは排他。末尾セミコロンを含む SQL は `Error::Driver` で停止する。
    #[arg(long, value_name = "SQL", conflicts_with_all = ["where_clause", "select"])]
    pub query: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct InfoArgs {
    /// 入力ファイルパス または URL（`pg://...` 等）。
    pub src: String,

    /// 入力 CRS が無いとき補完する EPSG。
    #[arg(long)]
    pub src_crs: Option<String>,

    /// 入力エンコーディング（`.cpg` 不在の Shapefile 等で有用）。
    #[arg(long)]
    pub encoding: Option<String>,
}

#[derive(clap::Args, Debug)]
pub struct SchemaArgs {
    /// 入力ファイルパス または URL（`pg://...` 等）。
    pub src: String,

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

/// `--insert-mode` の値。
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum InsertModeArg {
    /// driver が bulk_load 対応なら bulk、そうでなければ batch にフォールバックする。
    Auto,
    /// 必ず bulk 経路を使う。bulk 非対応 driver ではエラー。
    Bulk,
    /// 必ず batch (`LayerWriter::write_batch`) 経路を使う。
    Batch,
}
