# Driver 登録は inventory crate で行い、CLI で `use … as _;` を保持してリンクを強制する

Driver の登録に明示的な集中レジストリ（手書きの配列や登録関数）を使わず、**`inventory` crate による分散登録**を選んだ。各 driver crate は `&'static` の Driver インスタンスを `inventory::submit!` で送り、CLI 側は `inventory::iter` で起動時に 1 度だけ集約する。解決順がリンカ順に依存しないよう、集約後に `name()` で sort して決定的な並びにする。これにより driver crate を足すだけで登録が完結し、中央の登録リストを編集する必要がなくなる。

ただしこの方式には罠がある。リンカは「どこからも参照されていない」と判断した crate を `submit!` の副作用ごと strip するため、driver crate を `Cargo.toml` の依存に足すだけでは登録されない。これを防ぐため `crates/shpx-cli/src/registry.rs` は driver ごとに `use shpx_driver_<x> as _;` を 1 行保持し、リンクを強制している。**この一見不要な `use … as _;` は消すと該当 Driver が実行時に消える** — その意図をここに記録する（Driver 追加時は registry.rs と shpx-cli の Cargo.toml の両方を編集する）。

## Consequences

- `submit!` する Driver は `&'static`（`Box`/alloc 不要）にして、レジストリ登録をアロケーションフリーに保つ。
- 同一 scheme を複数 Driver が宣言しても、`name()` sort により解決順は決定的。
