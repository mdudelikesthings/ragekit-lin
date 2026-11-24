# ragekit

A native Linux tool for modding GTA V, in the spirit of OpenIV. It is written in Rust and does not use Wine.

This first milestone covers the archive layer, the foundation everything else builds on:

- Browse, search and extract any GTA V `.rpf` archive, including archives nested inside other archives (DLC packs). It reads every encryption type: none, OPEN, AES and NG.
- Build new archives from a folder, and add, replace or remove files in existing ones. Output is OpenIV-style **OPEN** archives, which the game loads through OpenIV.asi, OpenRPF or RageOpenV mods folders.
- Exported resources (`.ytd`, `.ydr`, `.yft`, `.ymap`, …) use the standard RSC7 form. That makes them interchangeable with OpenIV and CodeWalker exports.

Works with GTA V Legacy and Enhanced.

## Build

```sh
cargo build --release
# binary: target/release/ragekit
```

## Keys (one-time setup)

Rockstar's decryption keys can't be shipped, so ragekit takes them from **your own copy** of the game. They're stored in `~/.config/ragekit/keys`.

```sh
# 1. AES key from the executable on disk (point at the exe or the game folder)
ragekit keys extract --exe ~/.steam/steam/steamapps/common/"Grand Theft Auto V"

# 2. NG keys (most game archives use NG). Pick one:
#    a) start the game (Steam + Proton is fine), then scan its memory:
sudo ragekit --keys ~/.config/ragekit/keys keys extract --running
#    b) or decode CodeWalker's magic.dat (needs step 1 first):
ragekit keys extract --magic /path/to/CodeWalker.Core/Resources/magic.dat
#    c) or import an existing CodeWalker/GTAUtil keys folder:
ragekit keys extract --from-dir /path/to/Keys

ragekit keys status
```

Every key is checked against known SHA-1 hashes before it's saved.

## Use

```sh
G=~/.steam/steam/steamapps/common/"Grand Theft Auto V"

ragekit info  "$G/x64a.rpf"
ragekit ls    "$G/update/update.rpf" common/data -l
ragekit ls    "$G/update/x64/dlcpacks/mpbusiness/dlc.rpf" -r -n     # -n: into nested rpfs
ragekit find  "$G" 'adder*'                                        # search the whole game
ragekit cat   "$G/update/update.rpf" common/data/handling.meta > handling.meta
ragekit extract "$G/x64e.rpf" levels/gta5/vehicles.rpf -o out -x   # -x: unpack nested rpfs

# Modding: work on a copy in your mods folder, never the originals
mkdir -p "$G/mods/update" && cp "$G/update/update.rpf" "$G/mods/update/"
ragekit add "$G/mods/update/update.rpf" handling.meta --to common/data
ragekit add "$G/mods/x64e.rpf" adder.yft adder.ytd --to levels/gta5/vehicles.rpf
ragekit rm  "$G/mods/update/update.rpf" common/data/some.meta

# Build an archive from a folder (sub-folders named *.rpf become nested archives)
ragekit pack my_dlc/ -o dlc.rpf
```

Editing an archive in place keeps a `.bak` copy unless you pass `--no-backup`. Use `-o` to write the result somewhere else instead.

## Layout

| Crate | What |
|---|---|
| `crates/ragekit-rpf` | Library: crypto, keys, RPF7 reader (`Archive`), editor/writer (`Tree`) |
| `crates/ragekit-cli` | The `ragekit` command |

## Roadmap

1. ~~Archive core + CLI~~ (this release)
2. GUI file browser (Qt or egui) with drag-and-drop
3. Texture dictionaries: view and replace (`.ytd` ↔ DDS/PNG)
4. `.oiv` package installer
5. Meta/XML editors (`.ymt`, `.ymap`, `.ytyp` ↔ XML)
6. Model viewer (`.ydr`, `.yft`) with Vulkan
7. Writing `.ysc` scripts and NG-encrypted archives

## Known limits

- Output archives are OPEN (like OpenIV's). They need a mods-folder loader in the game. NG re-encryption isn't implemented yet.
- `.ysc` scripts can be extracted, and are carried along when an archive is rewritten. Importing new `.ysc` files isn't supported yet.
- Use mods in single-player / story mode only.

## Credits

The format details and the crypto routines come from [CodeWalker](https://github.com/dexyfex/CodeWalker) (GTACrypto/GTAKeys, MIT, © 2015 Neodymium) and the GTAMods wiki. `key_hashes.rs` holds only SHA-1 hashes of the key material, never the keys themselves.

Not affiliated with Rockstar Games, Take-Two or the OpenIV team.
