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

## Open issue: GTA V NG keys
`~/.config/ragekit/keys/` has `gtav_aes_key.dat`, `gtav_ng_key.dat` (27472 bytes = all 101 NG keys) and `gtav_hash_lut.dat`, but **no `gtav_ng_decrypt_tables.dat`** (the 272 NG decrypt tables), so `has_ng()` is false and NG archives won't open.
Next steps:
1. `ragekit keys status`, then `ragekit keys extract --exe "<GTA V folder>"` and look at the tables count.
2. With the game running (launched with `-nobattleye`): `ragekit keys extract --running`. Reading `/proc/<pid>/mem` needs `sudo sysctl kernel.yama.ptrace_scope=0` (Bazzite defaults to 1) or running as root. Check `find_game_pid` finds the Wine process (comm `GTA5.exe`).
3. Fallback: CodeWalker `magic.dat` via `ragekit keys extract --magic <file>`.
4. If tables still aren't found in exe/memory for 1.0.3889, inspect `Searcher` in `keys.rs` (table matching by SHA-1 of each 1 KiB table) against the current exe.

## Conventions
- Commit messages end with the Co-Authored-By/Claude-Session lines the harness provides.
- Keep README.md in sync with user-visible changes; screenshots in `docs/`.
