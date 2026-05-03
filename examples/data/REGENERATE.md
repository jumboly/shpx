# examples/data/ の再生成手順

このディレクトリの SHP fixture は git にコミット済みだが、内容を更新する場合
や別 CRS を追加したい場合は以下で再生成する。`cities.csv` / `lossy.csv` を
編集したら `cities.*` / `cities-3857.*` も同手順で再生成する。

各コマンドは `examples/*.sh` と同じく `SHPX_BIN` 環境変数で実行コマンドを
切り替えられる (既定は `cargo run --release -p shpx-cli --`)。

```bash
SHPX_BIN="${SHPX_BIN:-cargo run --release -p shpx-cli --}"
```

## cities.shp (5 都市 / EPSG:4326)

`cities.csv` をソースに、shpx convert で SHP へ変換する。

```bash
$SHPX_BIN convert examples/data/cities.csv examples/data/cities.shp \
  --src-crs EPSG:4326 --overwrite
```

## cities-3857.shp (Web Mercator 投影版)

`cities.shp` を再投影して書き出す (`examples/04-reproject.sh` で利用)。

```bash
$SHPX_BIN convert examples/data/cities.shp examples/data/cities-3857.shp \
  --reproject EPSG:3857 --overwrite
```

## lossy.csv

長い列名 (>10 bytes) を 3 つ持つ手書き CSV。`examples/05-on-loss.sh` で
`dbf-name-truncation` の `--on-loss={error,warn,skip}` を比較する素材。
内容を変える場合は `descrption_long_text` のように >10 bytes 列名を最低 1 つ
保つこと。
