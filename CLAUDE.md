# ragekit — notes for Claude Code

Native Linux OpenIV-style RAGE modding tool. One Rust binary: no args = GUI (egui/eframe 0.37), subcommands = CLI.

## Layout
- `crates/ragekit-core` — formats and logic: RPF7 (GTA V), RPF2/3/IMG3 (GTA IV), RPF4 (Max Payne 3), RPF6 (RDR, read-only), `tree.rs` (editable archive model), `keys.rs` (key extraction: exe hash search, /proc/<pid>/mem scan, CodeWalker magic.dat, probe search for MP3/RDR), `oiv/` (package installer), `meta.rs` (PSO/RSC7 Meta/RBF ⇄ XML), `textures.rs` (ytd + GTA IV wtd), `model.rs` + `rsc5.rs` (3D models), `mapdata.rs` (ide/ipl), `ysc.rs`, `tools.rs` (Script Hook V / ASI loader / RageOpenV / SHVDN / FusionFix installer), `steamcfg.rs` (Steam launch options), `dotnet.rs` (.NET 4.8 into Proton prefix), `games.rs` (install detection, mod setup check).
- `crates/ragekit` — `cli.rs`, `gui*.rs`, `ops.rs`, `settings.rs`.
- Tests: `cargo test --release` (70 pass). Real-sample tests need `RAGEKIT_SAMPLES=/path`.
- Keys are never shipped; `key_hashes.rs` holds SHA-1 hashes only. User keys live in `~/.config/ragekit/keys/`.

## This machine (Bazzite / Fedora Atomic, home is /var/home/Mdude)
- Steam: `~/.local/share/Steam`. GTA V Legacy (app 271590, GTA5.exe 1.0.3889.0) at `steamapps/common/Grand Theft Auto V`; prefix `steamapps/compatdata/271590/pfx`.
- Installed via ragekit tools: Ultimate ASI Loader (dinput8.dll), Script Hook V 3889.0, RageOpenV v1.0, SHVDN Enhanced v1.1.0.6. Also present: HeapAdjuster, PackfileLimitAdjuster, RAGEPluginHook files, an unrelated 2021 `XInput1_4.dll`.
- Launch fix found: the Rockstar launcher starts `GTA5_BE.exe`, which exits instantly under Proton ("Game exited before creating a window" in `pfx/drive_c/users/steamuser/Documents/Rockstar Games/Launcher/launcher.log`). Launch options should be `WINEDLLOVERRIDES="dinput8=n,b" %command% -nobattleye`.

## GTA V NG keys (solved 2026-11-25)
All four GTA V key files are in `~/.config/ragekit/keys/`, including `gtav_ng_decrypt_tables.dat` (278528 bytes), taken with `ragekit keys extract --running` while the game ran. NG archives (x64a.rpf, update.rpf) open.
- Root cause: CodeWalker's 272 table hashes have only 84 distinct values. `Searcher` kept one slot per hash, so at most 84 slots could fill and tables never completed. Fixed: each hash maps to every slot it fills.
- The tables are not in GTA5.exe on disk (packed); they only appear in memory while the game runs. `ptrace_scope` is already 0 on this machine, so no sudo is needed.

## Building on this machine (15 GB RAM)
A full `cargo build --release` (opt-level 3) runs out of memory on ragekit-core (>12 GB) and the OOM killer takes the Claude app with it. Build in a capped unit at opt-level 1 (peak ~1.6 GB):
`systemd-run --user --wait --collect -p MemoryMax=8G -E PATH="$HOME/.cargo/bin:/usr/bin" -E CARGO_PROFILE_RELEASE_LTO=false -E CARGO_PROFILE_RELEASE_OPT_LEVEL=1 --working-directory="$PWD" ~/.cargo/bin/cargo build --release -j 2`
Rust is installed via rustup in `~/.cargo`.

## Conventions
- Commit messages end with the Co-Authored-By/Claude-Session lines the harness provides.
- Keep README.md in sync with user-visible changes; screenshots in `docs/`.
