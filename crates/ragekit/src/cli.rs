//! Command-line interface.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use ragekit_core::tree::node_from_disk;
use ragekit_core::{normalize_path, vfs, Format, Keys, RowKind, Tree};

use crate::ops::{self, human, join};

#[derive(Parser)]
#[command(
    name = "ragekit",
    version,
    about = "GTA V and GTA IV modding tool for Linux. Run without a command to open the browser."
)]
pub struct Cli {
    /// Folder holding key files (default: ~/.config/ragekit/keys or $RAGEKIT_KEYS)
    #[arg(long, global = true)]
    pub keys: Option<PathBuf>,
    #[command(subcommand)]
    pub cmd: Option<Cmd>,
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Open the graphical browser (optionally at a game folder or archive)
    Gui { path: Option<PathBuf> },
    /// Manage decryption keys taken from your own games
    #[command(subcommand)]
    Keys(KeysCmd),
    /// Show an archive's summary
    Info { archive: PathBuf },
    /// List contents. PATH may go inside nested archives (e.g. dlc.rpf/x64)
    Ls {
        archive: PathBuf,
        path: Option<String>,
        /// List everything below PATH
        #[arg(short, long)]
        recursive: bool,
        /// Show sizes and types
        #[arg(short, long)]
        long: bool,
        /// With -r, also list inside nested archives
        #[arg(short = 'n', long)]
        nested: bool,
    },
    /// Extract files or folders (everything if no PATH is given)
    Extract {
        archive: PathBuf,
        paths: Vec<String>,
        /// Output folder
        #[arg(short, long, default_value = ".")]
        out: PathBuf,
        /// Unpack nested .rpf archives into folders
        #[arg(short = 'x', long)]
        expand_rpf: bool,
        /// Also save binary metadata as .xml and .gxt2 text tables as .txt
        #[arg(long)]
        convert: bool,
    },
    /// Print one file to stdout (handy for .meta / .xml / .dat)
    Cat { archive: PathBuf, path: String },
    /// Show binary metadata (PSO .ymt/.ymf, RSC7 .ytyp/.ymap, RBF) as XML and
    /// .gxt2 text tables as text, or replace them from an edited XML/text file
    Xml {
        /// A metadata file on disk, or an archive
        file: PathBuf,
        /// Path inside the archive
        path: Option<String>,
        /// Convert this XML file back and replace the metadata with it
        #[arg(long)]
        import: Option<PathBuf>,
        /// Extra names to resolve hashes (comma-separated)
        #[arg(long, value_delimiter = ',')]
        names: Vec<String>,
        #[arg(long)]
        no_backup: bool,
    },
    /// 3D models (.ydr/.ydd/.yft, .wdr/.wdd/.wft): info, export to OBJ
    /// (with textures) or CodeWalker XML
    Model {
        /// A model on disk, or an archive
        file: PathBuf,
        /// Path of the model inside the archive
        path: Option<String>,
        /// Write a Wavefront OBJ (plus .mtl and the embedded textures as PNG)
        #[arg(long)]
        obj: Option<PathBuf>,
        /// Write CodeWalker XML (.ydr only; embedded textures go beside it as .dds)
        #[arg(long)]
        xml: Option<PathBuf>,
        /// Build FILE (a .ydr) from CodeWalker XML instead (textures are read
        /// as .dds files from the XML's folder)
        #[arg(long, conflicts_with_all = ["obj", "xml", "path"])]
        from_xml: Option<PathBuf>,
    },
    /// Compiled scripts (.ysc): header, native calls and strings
    Script {
        /// A .ysc on disk, or an archive
        file: PathBuf,
        /// Path of the script inside the archive
        path: Option<String>,
        /// Also save the raw bytecode here
        #[arg(long)]
        code: Option<PathBuf>,
    },
    /// Texture dictionaries (.ytd, GTA IV .wtd): list, export, replace, add, remove
    Tex {
        /// A .ytd / .wtd on disk, or an archive
        file: PathBuf,
        /// Path of the dictionary inside the archive
        path: Option<String>,
        /// Export every texture into this folder
        #[arg(long)]
        export: Option<PathBuf>,
        /// Export as DDS instead of PNG
        #[arg(long)]
        dds: bool,
        /// Replace a texture: NAME=image.png (or .dds/.jpg/.tga…)
        #[arg(long, value_name = "NAME=FILE")]
        replace: Vec<String>,
        /// Add textures from image files (named after the file)
        #[arg(long)]
        add: Vec<PathBuf>,
        /// Remove textures by name
        #[arg(long)]
        remove: Vec<String>,
        #[arg(long)]
        no_backup: bool,
    },
    /// Build a new archive from a folder
    Pack {
        folder: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// v = GTA V RPF7 (default), iv = GTA IV RPF2, img = GTA IV IMG3,
        /// mp3 = Max Payne 3 RPF4.
        /// Inferred from an .img output name.
        #[arg(short, long)]
        format: Option<String>,
    },
    /// Add or replace files/folders in an archive
    Add {
        archive: PathBuf,
        #[arg(required = true)]
        sources: Vec<PathBuf>,
        /// Folder inside the archive to put them in (may go into nested archives)
        #[arg(short, long, default_value = "")]
        to: String,
        /// Write the result here instead of modifying the archive in place
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Don't keep a .bak copy when modifying in place
        #[arg(long)]
        no_backup: bool,
    },
    /// Remove files/folders from an archive
    Rm {
        archive: PathBuf,
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        no_backup: bool,
    },
    /// Rename a file or folder inside an archive
    Mv {
        archive: PathBuf,
        path: String,
        new_name: String,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        no_backup: bool,
    },
    /// Install, list and uninstall OpenIV packages (.oiv)
    #[command(subcommand)]
    Oiv(OivCmd),
    /// List GTA V / GTA IV installs found on this computer
    Games,
    /// Build an add-on vehicle pack from a folder of .yft/.ytd/.meta files
    Addon {
        folder: PathBuf,
        /// Pack name (default: the folder name)
        #[arg(short, long)]
        name: Option<String>,
        /// Save a shareable .oiv package
        #[arg(long)]
        oiv: Option<PathBuf>,
        /// Install into this game folder now (into mods/, uninstallable)
        #[arg(long)]
        install: Option<PathBuf>,
    },
    /// Jenkins (joaat) hash of names, as the game uses for models and labels
    Hash {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Check a GTA V folder's mod setup (ASI loader, mods loader, Proton launch options)
    Setup {
        #[arg(short, long)]
        game: Option<PathBuf>,
    },
    /// Search names across a game folder or archive, including inside archives.
    /// PATTERN is case-insensitive and may use * and ?
    Find { root: PathBuf, pattern: String },
}

#[derive(Subcommand)]
pub enum OivCmd {
    /// Show what a package contains and does
    Info { package: PathBuf },
    /// Install a package (GTA V: into the mods folder by default)
    Install {
        package: PathBuf,
        /// Game folder (default: the last one opened in ragekit)
        #[arg(short, long)]
        game: Option<PathBuf>,
        /// GTA V: change the game's own files instead of the mods folder
        #[arg(long)]
        no_mods: bool,
        /// Only show what would happen
        #[arg(short = 'n', long)]
        dry_run: bool,
        /// Install even if some steps failed (they are skipped)
        #[arg(long)]
        force: bool,
    },
    /// List installed packages
    List {
        #[arg(short, long)]
        game: Option<PathBuf>,
    },
    /// Undo an install (ID from `ragekit oiv list`)
    Uninstall {
        id: String,
        #[arg(short, long)]
        game: Option<PathBuf>,
        /// Uninstall even if the files changed since
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Show which keys are installed
    Status,
    /// Find keys in your games and save them. Combine sources as needed.
    Extract {
        /// GTA5.exe / GTAIV.exe / EFLC.exe / MaxPayne3.exe, or a game folder
        #[arg(long)]
        exe: Vec<PathBuf>,
        /// An encrypted archive of the game to test candidate keys against
        /// (Max Payne 3 / RDR). Found in the game folder if not given.
        #[arg(long)]
        archive: Option<PathBuf>,
        /// Scan a running game process (finds all GTA V keys; needs ptrace rights)
        #[arg(long)]
        pid: Option<u32>,
        /// Find the running game automatically and scan it
        #[arg(long)]
        running: bool,
        /// CodeWalker's magic.dat (needs the GTA V AES key; supplies the NG keys)
        #[arg(long)]
        magic: Option<PathBuf>,
        /// Import key files from a CodeWalker/GTAUtil keys folder
        #[arg(long)]
        from_dir: Option<PathBuf>,
    },
}

pub fn run(cli: &Cli, cmd: &Cmd) -> Result<()> {
    let kdir = ops::keys_dir(cli.keys.as_deref());
    let keys = || ops::load_keys(&kdir);
    match cmd {
        Cmd::Gui { .. } => unreachable!("handled in main"),
        Cmd::Keys(k) => keys_cmd(&kdir, k),
        Cmd::Info { archive } => {
            let t = open(archive, keys()?)?;
            let (dirs, files) = t.counts();
            let mut nested = 0;
            let mut total = 0u64;
            t.for_each_file(&mut |_, f| {
                total += f.size();
                if matches!(f, ragekit_core::FileData::Rpf(_)) {
                    nested += 1;
                }
            });
            println!("Archive:      {}", archive.display());
            println!("Game:         {}", t.format.game());
            println!("Format:       {}", t.origin);
            println!("Size:         {}", human(fs::metadata(archive)?.len()));
            println!("Folders:      {dirs}");
            println!("Files:        {files}");
            if nested > 0 {
                println!("Nested RPFs:  {nested}");
            }
            println!("Unpacked:     {}", human(total));
            Ok(())
        }
        Cmd::Ls { archive, path, recursive, long, nested } => {
            let t = open(archive, keys()?)?;
            let base = normalize_path(path.as_deref().unwrap_or(""));
            let mut out = std::io::stdout().lock();
            if t.kind(&base)? == RowKind::File {
                let name = base.rsplit('/').next().unwrap_or("");
                let rows = t.list(base.rsplit_once('/').map(|x| x.0).unwrap_or(""))?;
                if let Some(r) = rows.iter().find(|r| r.name.eq_ignore_ascii_case(name)) {
                    print_row(&mut out, r, &r.name, *long)?;
                }
                return Ok(());
            }
            list(&t, &base, "", *recursive, *long, *nested, &mut out)
        }
        Cmd::Extract { archive, paths, out, expand_rpf, convert } => {
            let t = open(archive, keys()?)?;
            fs::create_dir_all(out)?;
            let targets = if paths.is_empty() { vec![String::new()] } else { paths.clone() };
            let (mut n, mut failed) = (0, Vec::new());
            for p in targets {
                let p = normalize_path(&p);
                let dest = if p.is_empty() { out.clone() } else { out.join(p.rsplit('/').next().unwrap()) };
                let (c, f) = ops::extract_opts(&t, &p, &dest, &ops::ExtractOpts { expand: *expand_rpf, convert: *convert }, &mut |_| {})?;
                n += c;
                failed.extend(f);
            }
            for f in &failed {
                eprintln!("{f}");
            }
            eprintln!("Extracted {n} files to {}", out.display());
            if !failed.is_empty() {
                bail!("{} files could not be extracted", failed.len());
            }
            Ok(())
        }
        Cmd::Cat { archive, path } => {
            let t = open(archive, keys()?)?;
            std::io::stdout().lock().write_all(&t.read(path)?)?;
            Ok(())
        }
        Cmd::Xml { file, path, import, names, no_backup } => {
            use ragekit_core::meta;
            let in_archive = path.is_some();
            let (mut tree, data) = if let Some(p) = path {
                let t = open(file, keys()?)?;
                let d = t.read(p)?;
                (Some(t), d)
            } else {
                (None, fs::read(file)?)
            };
            let conv = ragekit_core::convert::kind(&data).ok_or_else(|| anyhow!("not binary metadata (PSO, RSC7 Meta, RBF) or a .gxt2 text table"))?;
            match import {
                None => {
                    let x = match conv {
                        ragekit_core::convert::Conv::Meta(_) => meta::to_xml(&data, names)?.0,
                        _ => ragekit_core::convert::to_text(&data)?.0,
                    };
                    eprintln!("({} file)", conv.label());
                    std::io::stdout().lock().write_all(x.as_bytes())?;
                }
                Some(xml_path) => {
                    let bytes = ragekit_core::convert::from_text(&fs::read_to_string(xml_path)?, &data)?;
                    if in_archive {
                        let t = tree.as_mut().unwrap();
                        t.insert_bytes(path.as_deref().unwrap(), bytes)?;
                        finish(t, file, None, *no_backup)?;
                    } else {
                        let (n, bak) = {
                            let mut bak = None;
                            if !no_backup {
                                let mut s = file.as_os_str().to_owned();
                                s.push(".bak");
                                let b = PathBuf::from(s);
                                if !b.exists() {
                                    fs::copy(file, &b)?;
                                    bak = Some(b);
                                }
                            }
                            (ops::write_atomic(file, |w| {
                                w.write_all(&bytes)?;
                                Ok(bytes.len() as u64)
                            })?, bak)
                        };
                        if let Some(b) = bak {
                            eprintln!("Backup: {}", b.display());
                        }
                        eprintln!("Wrote {} ({})", file.display(), human(n));
                    }
                }
            }
            Ok(())
        }
        Cmd::Model { file, path, obj, xml, from_xml } => {
            use ragekit_core::{model, textures};
            if let Some(x) = from_xml {
                let text = fs::read_to_string(x)?;
                let bytes = model::ydr_from_xml(&text, x.parent())?;
                let m = model::load("x.ydr", &bytes)?;
                fs::write(file, &bytes)?;
                println!("Wrote {} ({}, {} triangles, {} embedded textures)", file.display(), human(bytes.len() as u64), m.triangles(), m.textures.len());
                return Ok(());
            }
            let (name, data) = input(file, path.as_deref(), &keys)?;
            let m = model::load(&name, &data)?;
            println!("{} model: {} meshes, {} triangles, {} vertices, {} embedded textures", m.game, m.meshes.len(), m.triangles(), m.vertices(), m.textures.len());
            if let Some((lo, hi)) = m.bounds() {
                println!("Bounds: ({:.2}, {:.2}, {:.2}) to ({:.2}, {:.2}, {:.2})", lo[0], lo[1], lo[2], hi[0], hi[1], hi[2]);
            }
            for n in &m.notes {
                println!("  {n}");
            }
            if let Some(o) = obj {
                let stem = o.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "model".into());
                let mtl = format!("{stem}.mtl");
                let (text, mtl_text) = m.to_obj(&mtl);
                let dir = o.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(std::path::Path::new("."));
                fs::create_dir_all(dir)?;
                fs::write(o, text)?;
                fs::write(dir.join(&mtl), mtl_text)?;
                for t in &m.textures {
                    if let Ok(png) = textures::to_png(t) {
                        fs::write(dir.join(format!("{}.png", ops::safe_name(&t.name)?)), png)?;
                    }
                }
                eprintln!("Wrote {} (+ {mtl} and {} textures)", o.display(), m.textures.len());
            }
            if let Some(x) = xml {
                if !name.to_ascii_lowercase().ends_with(".ydr") {
                    bail!("CodeWalker XML export is for .ydr files");
                }
                let text = model::ydr_to_xml(&data, x.parent().filter(|p| !p.as_os_str().is_empty()).or(Some(std::path::Path::new("."))))?;
                fs::write(x, text)?;
                eprintln!("Wrote {}", x.display());
            }
            Ok(())
        }
        Cmd::Script { file, path, code } => {
            let (_, data) = input(file, path.as_deref(), &keys)?;
            let sc = ragekit_core::ysc::parse(&data)?;
            print!("{}", sc.report());
            if let Some(c) = code {
                fs::write(c, &sc.code)?;
                eprintln!("Wrote {} bytes of bytecode to {}", sc.code.len(), c.display());
            }
            Ok(())
        }
        Cmd::Tex { file, path, export, dds, replace, add, remove, no_backup } => {
            use ragekit_core::textures;
            let mut tree = None;
            let data = match path {
                Some(p) => {
                    let t = open(file, keys()?)?;
                    let d = t.read(p)?;
                    tree = Some(t);
                    d
                }
                None => fs::read(file)?,
            };
            let mut dict = textures::load(&data)?;
            if let Some(dir) = export {
                fs::create_dir_all(dir)?;
                let mut n = 0;
                for t in &dict.textures {
                    let (bytes, ext) = if *dds { (textures::to_dds(t), "dds") } else {
                        match textures::to_png(t) {
                            Ok(b) => (b, "png"),
                            Err(e) => {
                                eprintln!("{}: {e} (exporting as DDS instead)", t.name);
                                (textures::to_dds(t), "dds")
                            }
                        }
                    };
                    fs::write(dir.join(format!("{}.{ext}", ops::safe_name(&t.name)?)), bytes)?;
                    n += 1;
                }
                eprintln!("Exported {n} textures to {}", dir.display());
            }
            let editing = !replace.is_empty() || !add.is_empty() || !remove.is_empty();
            if editing {
                for r in replace {
                    let (name, img) = r.split_once('=').ok_or_else(|| anyhow!("--replace takes NAME=FILE"))?;
                    dict.replace(name, &fs::read(img)?)?;
                    eprintln!("replaced {name}");
                }
                for a in add {
                    let name = a.file_stem().ok_or_else(|| anyhow!("bad file {}", a.display()))?.to_string_lossy().to_string();
                    dict.add(&name, &fs::read(a)?)?;
                    eprintln!("added    {name}");
                }
                for r in remove {
                    dict.remove(r)?;
                    eprintln!("removed  {r}");
                }
                let bytes = dict.save()?;
                match (tree.as_mut(), path) {
                    (Some(t), Some(p)) => {
                        t.insert_bytes(p, bytes)?;
                        finish(t, file, None, *no_backup)?;
                    }
                    _ => {
                        if !no_backup {
                            let mut s = file.as_os_str().to_owned();
                            s.push(".bak");
                            let b = PathBuf::from(s);
                            if !b.exists() {
                                fs::copy(file, &b)?;
                                eprintln!("Backup: {}", b.display());
                            }
                        }
                        ops::write_atomic(file, |w| {
                            w.write_all(&bytes)?;
                            Ok(bytes.len() as u64)
                        })?;
                        eprintln!("Wrote {}", file.display());
                    }
                }
            }
            if export.is_none() && !editing {
                println!("{}{}", if dict.enhanced { "Enhanced (gen9) " } else { "" }, "texture dictionary");
                for t in &dict.textures {
                    println!("{:<32} {:>5}x{:<5} {:<11} {} mips", t.name, t.width, t.height, textures::format_name(t.format), t.levels);
                }
            }
            Ok(())
        }
        Cmd::Pack { folder, out, format } => {
            let fmt = match format.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("v") | Some("gta5") | Some("rpf7") => Format::Rpf7,
                Some("iv") | Some("gta4") | Some("rpf2") => Format::Rpf2,
                Some("img") | Some("img3") => Format::Img3,
                Some("mp3") | Some("rpf4") => Format::Rpf4,
                Some(other) => bail!("unknown format '{other}' (use v, iv, img or mp3)"),
                None if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("img")) => Format::Img3,
                None => Format::Rpf7,
            };
            if !folder.is_dir() {
                bail!("{} is not a folder", folder.display());
            }
            let tree = Tree::from_dir(folder, fmt, keys()?)?;
            let n = ops::write_atomic(out, |w| Ok(tree.write(w)?))?;
            eprintln!("Wrote {} ({}, {} {})", out.display(), human(n), fmt.game(), fmt.label());
            Ok(())
        }
        Cmd::Add { archive, sources, to, out, no_backup } => {
            let keys = keys()?;
            let mut t = open(archive, keys.clone())?;
            let base = normalize_path(to);
            for s in sources {
                let name = s
                    .file_name()
                    .ok_or_else(|| anyhow!("bad source path {}", s.display()))?
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let dest = join(&base, &name);
                let fmt = t.format_at(&dest);
                let node = node_from_disk(s, fmt, &keys).with_context(|| format!("reading {}", s.display()))?;
                let existed = t.contains(&dest);
                t.insert_node(&dest, node)?;
                eprintln!("{} {dest}", if existed { "replaced" } else { "added   " });
            }
            finish(&t, archive, out.as_ref(), *no_backup)
        }
        Cmd::Rm { archive, paths, out, no_backup } => {
            let mut t = open(archive, keys()?)?;
            for p in paths {
                t.remove(p).with_context(|| format!("removing {p}"))?;
                eprintln!("removed {}", normalize_path(p));
            }
            finish(&t, archive, out.as_ref(), *no_backup)
        }
        Cmd::Mv { archive, path, new_name, out, no_backup } => {
            let mut t = open(archive, keys()?)?;
            t.rename(path, new_name)?;
            finish(&t, archive, out.as_ref(), *no_backup)
        }
        Cmd::Oiv(o) => oiv_cmd(o, keys),
        Cmd::Hash { names } => {
            for n in names {
                let h = ragekit_core::crypto::joaat(n);
                println!("{n:<32} 0x{h:08X}  {h}  {}", h as i32);
            }
            Ok(())
        }
        Cmd::Addon { folder, name, oiv, install } => {
            use ragekit_core::dlc;
            let name = name.clone().unwrap_or_else(|| folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
            let pack = dlc::scan(folder, &name)?;
            eprintln!("Pack \"{}\": {} streamed file(s), {} data file(s)", pack.name, pack.stream.len(), pack.data.len());
            for (f, _, t) in &pack.data {
                eprintln!("  {f:<28} {t}");
            }
            for s in &pack.skipped {
                eprintln!("  skipped {s}");
            }
            let keys = keys()?;
            if let Some(out) = oiv {
                dlc::write_oiv(&pack, out, keys.clone())?;
                eprintln!("Wrote {}", out.display());
            }
            if let Some(game) = install {
                let tmp = std::env::temp_dir().join(format!("ragekit-addon-{}", std::process::id()));
                let _ = fs::remove_dir_all(&tmp);
                dlc::write_package_dir(&pack, &tmp, keys.clone())?;
                let pkg = ragekit_core::oiv::Package::open(&tmp)?;
                let plan = ragekit_core::oiv::plan(&pkg, &ragekit_core::oiv::Options { game_dir: game.clone(), use_mods: true }, keys);
                for l in &plan.log {
                    eprintln!("  {}", l.msg);
                }
                let rec = plan.commit(&pkg, false, &mut |_| {})?;
                let _ = fs::remove_dir_all(&tmp);
                eprintln!("Installed (undo with: ragekit oiv uninstall {} --game \"{}\")", rec.id, game.display());
            }
            if oiv.is_none() && install.is_none() {
                eprintln!("Nothing written: add --oiv FILE and/or --install GAME_FOLDER");
            }
            Ok(())
        }
        Cmd::Games => {
            let found = ragekit_core::games::find_installs();
            if found.is_empty() {
                println!("No installs found in Steam, Heroic or Lutris folders.");
            }
            for i in found {
                println!("{:<32} {}", i.label(), i.path.display());
            }
            Ok(())
        }
        Cmd::Setup { game } => {
            let g = game_dir(game)?;
            let s = ragekit_core::games::check_mod_setup(&g);
            let f = |o: &Option<String>| o.clone().unwrap_or_else(|| "missing".into());
            println!("GTA V {} at {}", if s.enhanced { "Enhanced" } else { "Legacy" }, g.display());
            println!("ASI loader:         {}", f(&s.asi_loader));
            println!("Mods-folder loader: {}", f(&s.rpf_loader));
            println!("Script Hook V:      {}", if s.script_hook { "found" } else { "missing" });
            println!("mods folder:        {}", if s.mods_folder { format!("{} file(s)", s.mods_files) } else { "not created".into() });
            println!("ASI plugins:        {}", if s.asi_plugins.is_empty() { "none".into() } else { s.asi_plugins.join(", ") });
            for p in &s.problems {
                println!("\nPROBLEM: {p}");
            }
            if let Some(o) = &s.launch_option {
                println!("\nSteam launch options for Proton:\n  {o}");
            }
            for t in &s.tips {
                println!("\n- {t}");
            }
            Ok(())
        }
        Cmd::Find { root, pattern } => {
            let mut out = std::io::stdout().lock();
            let mut hits = 0usize;
            let cancel = AtomicBool::new(false);
            vfs::search(
                root,
                pattern,
                keys()?,
                &cancel,
                &mut |h| {
                    let f = h.file.strip_prefix(root).unwrap_or(&h.file).display().to_string();
                    let f = if f.is_empty() { h.file.display().to_string() } else { f };
                    let _ = if h.inner.is_empty() { writeln!(out, "{f}") } else { writeln!(out, "{f}/{}", h.inner) };
                    hits += 1;
                },
                &mut |e| eprintln!("skipping {e}"),
            );
            eprintln!("{hits} matches");
            Ok(())
        }
    }
}

fn open(path: &std::path::Path, keys: Arc<Keys>) -> Result<Tree> {
    vfs::open(path, keys).with_context(|| format!("opening {}", path.display()))
}

fn finish(t: &Tree, archive: &std::path::Path, out: Option<&PathBuf>, no_backup: bool) -> Result<()> {
    let target = out.map(|p| p.as_path()).unwrap_or(archive);
    let (n, bak) = ops::save(t, target, out.is_none() && !no_backup)?;
    if let Some(b) = bak {
        eprintln!("Backup: {}", b.display());
    }
    eprintln!("Wrote {} ({}, {})", target.display(), human(n), t.format.label());
    Ok(())
}

fn print_row(out: &mut impl Write, r: &ragekit_core::Row, label: &str, long: bool) -> Result<()> {
    let shown = if r.kind == RowKind::Folder { format!("{label}/") } else { label.to_string() };
    if !long {
        writeln!(out, "{shown}")?;
        return Ok(());
    }
    let size = if r.kind == RowKind::Folder { r.detail.clone() } else { human(r.size) };
    let desc = if r.kind == RowKind::Folder { "" } else { ops::describe(&r.name) };
    writeln!(out, "{:<8} {size:>10}  {shown}  {desc}", r.type_label)?;
    Ok(())
}

fn list(t: &Tree, path: &str, prefix: &str, rec: bool, long: bool, nested: bool, out: &mut impl Write) -> Result<()> {
    for r in t.list(path)? {
        let label = join(prefix, &r.name);
        print_row(out, &r, &label, long)?;
        let enter = r.kind == RowKind::Folder || (nested && r.kind == RowKind::Archive);
        if rec && enter {
            let p = join(path, &r.name);
            if let Err(e) = list(t, &p, &label, rec, long, nested, out) {
                eprintln!("{label}: {e:#}");
            }
        }
    }
    Ok(())
}

fn keys_cmd(dir: &std::path::Path, k: &KeysCmd) -> Result<()> {
    let yes = |b: bool| if b { "yes" } else { "no" };
    match k {
        KeysCmd::Status => {
            let keys = Keys::load_dir(dir)?;
            println!("Key folder: {}", dir.display());
            println!("GTA V   AES key:      {}", yes(keys.aes.is_some()));
            println!("GTA V   NG keys:      {}", yes(keys.ng_keys.is_some()));
            println!("GTA V   NG tables:    {}", yes(keys.ng_tables.is_some()));
            println!("GTA V   hash lookup:  {}", yes(keys.hash_lut.is_some()));
            println!("GTA IV  AES key:      {}", yes(keys.iv_aes.is_some()));
            println!("Max Payne 3 AES key:  {}", yes(keys.mp3_aes.is_some()));
            println!("RDR     AES key:      {}", yes(keys.rdr_aes.is_some()));
            Ok(())
        }
        KeysCmd::Extract { exe, archive, pid, running, magic, from_dir } => {
            let mut keys = Keys::load_dir(dir).unwrap_or_default();
            if let Some(d) = from_dir {
                keys.merge(Keys::load_dir(d).with_context(|| format!("importing {}", d.display()))?);
                eprintln!("Imported keys from {}", d.display());
            }
            // Max Payne 3 / RDR keys are found by testing candidates against
            // one of the game's own encrypted archives.
            let probe_from = |dir: Option<&std::path::Path>| -> Result<Option<(PathBuf, vfs::KeyFor, vfs::KeyProbe)>> {
                if let Some(a) = archive {
                    let mut head = vec![0u8; 0x810];
                    use std::io::Read;
                    fs::File::open(a)?.read_exact(&mut head).with_context(|| format!("reading {}", a.display()))?;
                    let (k, p) = vfs::key_probe(&head)
                        .ok_or_else(|| anyhow!("{} is not an encrypted RPF2/RPF4/RPF6 archive", a.display()))?;
                    return Ok(Some((a.clone(), k, p)));
                }
                Ok(dir.and_then(|d| vfs::find_probe_archive(d, None)))
            };
            let mut probe_saved: Option<(PathBuf, vfs::KeyFor, vfs::KeyProbe)> = None;
            for e in exe {
                let exe = ops::resolve_exe(e)?;
                eprintln!("Searching {} ...", exe.display());
                let data = fs::read(&exe)?;
                let mp3 = vfs::detect_game(exe.parent().unwrap_or(std::path::Path::new("."))).is_some_and(|(g, _)| g == vfs::Game::MaxPayne3);
                if !mp3 {
                    report(keys.search_bytes(&data));
                }
                if let Some((arch, which, probe)) = probe_from(exe.parent())? {
                    if which != vfs::KeyFor::GtaIV || !keys.has_iv() {
                        eprintln!("  testing keys against {} ...", arch.display());
                        match Keys::find_by_probe(&data, &*probe) {
                            Some(k) => {
                                *which.slot(&mut keys) = Some(k);
                                eprintln!("  {} AES key: found", which.label());
                            }
                            None => eprintln!(
                                "  {} AES key: not in the file (it may be packed); start the game and use --running",
                                which.label()
                            ),
                        }
                    }
                    probe_saved = Some((arch, which, probe));
                }
            }
            let pid = match (pid, running) {
                (Some(p), _) => Some(*p),
                (None, true) => Some(ops::find_game_pid().ok_or_else(|| anyhow!("no running game found"))?),
                _ => None,
            };
            if let Some(p) = pid {
                if probe_saved.is_none() {
                    // The game folder is the process's exe folder under Wine/Proton.
                    let dir = ops::process_game_dir(p);
                    probe_saved = probe_from(dir.as_deref())?;
                }
                match &probe_saved {
                    Some((_, which, probe)) if which.slot(&mut keys).is_none() => {
                        eprintln!("Testing memory of process {p} for the {} key ...", which.label());
                        match Keys::find_by_probe_in_process(p, &**probe)? {
                            Some(k) => {
                                *which.slot(&mut keys) = Some(k);
                                eprintln!("  {} AES key: found", which.label());
                            }
                            None => eprintln!("  {} AES key: not found", which.label()),
                        }
                    }
                    _ => {
                        eprintln!("Scanning memory of process {p} ...");
                        report(keys.search_process(p)?);
                    }
                }
            }
            if let Some(m) = magic {
                keys.load_codewalker_magic(&fs::read(m)?).context("decoding magic.dat")?;
                eprintln!("Decoded GTA V NG keys from {}", m.display());
            }
            keys.verify()?;
            keys.save_dir(dir)?;
            println!("Saved to {}", dir.display());
            println!(
                "GTA V: AES {}  NG {}   GTA IV: {}   Max Payne 3: {}   RDR: {}",
                yes(keys.has_aes()),
                yes(keys.has_ng()),
                yes(keys.has_iv()),
                yes(keys.mp3_aes.is_some()),
                yes(keys.rdr_aes.is_some())
            );
            if keys.has_aes() && !keys.has_ng() {
                println!(
                    "GTA V NG keys are still missing. Start GTA V and run\n  \
                     sudo ragekit --keys {} keys extract --running\nor use --magic with CodeWalker's magic.dat.",
                    dir.display()
                );
            }
            Ok(())
        }
    }
}

/// A file's name and bytes: from disk, or from inside an archive.
fn input(file: &std::path::Path, path: Option<&str>, keys: &dyn Fn() -> Result<Arc<Keys>>) -> Result<(String, Vec<u8>)> {
    match path {
        Some(p) => {
            let t = open(file, keys()?)?;
            Ok((p.rsplit('/').next().unwrap_or(p).to_string(), t.read(p)?))
        }
        None => Ok((file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), fs::read(file)?)),
    }
}

fn report(r: ragekit_core::keys::SearchReport) {
    eprintln!(
        "  GTA V AES: {}  hash table: {}  NG keys: {}/101  NG tables: {}/272   GTA IV AES: {}",
        if r.aes { "yes" } else { "no" },
        if r.hash_lut { "yes" } else { "no" },
        r.ng_keys_found,
        r.ng_tables_found,
        if r.iv_aes { "yes" } else { "no" },
    );
}

fn game_dir(g: &Option<PathBuf>) -> Result<PathBuf> {
    g.clone()
        .or_else(ops::load_last_root)
        .ok_or_else(|| anyhow!("no game folder given; use --game /path/to/game"))
}

fn print_package(p: &ragekit_core::oiv::Package) {
    let m = &p.meta;
    println!("{} {}", m.name, m.version);
    if !m.author.is_empty() {
        println!("by {}", m.author);
    }
    println!("For:      {}{}", if m.is_gta5() { "GTA V" } else { "GTA IV" }, m.game_version.as_ref().map(|g| format!(" ({g})")).unwrap_or_default());
    println!("Format:   OIV {}", m.format_version);
    if !m.description.is_empty() {
        println!("\n{}\n", m.description.trim());
    }
    if !m.license.is_empty() {
        println!("License: {}\n", m.license.trim());
    }
    println!("Steps:");
    print_ops(&p.ops, 1);
}

fn print_ops(ops: &[ragekit_core::oiv::Op], depth: usize) {
    use ragekit_core::oiv::Op;
    let pad = "  ".repeat(depth);
    for op in ops {
        match op {
            Op::Archive { path, create, children, .. } => {
                println!("{pad}archive {path}{}", if *create { " (created if missing)" } else { "" });
                print_ops(children, depth + 1);
            }
            Op::Add { source, dest } => println!("{pad}add     {dest}  <- {source}"),
            Op::Delete { target } => println!("{pad}delete  {target}"),
            Op::Text { path, edits, .. } => println!("{pad}text    {path} ({} edits)", edits.len()),
            Op::Xml { path, pso, edits } => println!("{pad}{}     {path} ({} edits)", if *pso { "pso" } else { "xml" }, edits.len()),
            Op::Defragmentation => println!("{pad}defragment"),
        }
    }
}

fn oiv_cmd(o: &OivCmd, keys: impl Fn() -> Result<Arc<Keys>>) -> Result<()> {
    use ragekit_core::oiv::{self, Level, Options, Package};
    match o {
        OivCmd::Info { package } => {
            print_package(&Package::open(package)?);
            Ok(())
        }
        OivCmd::Install { package, game, no_mods, dry_run, force } => {
            let game = game_dir(game)?;
            let pkg = Package::open(package)?;
            let plan = oiv::plan(&pkg, &Options { game_dir: game.clone(), use_mods: !no_mods }, keys()?);
            for l in &plan.log {
                let tag = match l.level {
                    Level::Info => "      ",
                    Level::Warn => "WARN  ",
                    Level::Error => "ERROR ",
                };
                eprintln!("{tag}{}", l.msg);
            }
            eprintln!("\n{} error(s), {} warning(s). Files to change:", plan.errors(), plan.warnings());
            for (f, write) in plan.changes() {
                eprintln!("  {} {f}", if write { "write " } else { "delete" });
            }
            if *dry_run {
                return Ok(());
            }
            if plan.errors() > 0 && !force {
                bail!("not installed because of the errors above (use --force to skip the failed steps)");
            }
            let rec = plan.commit(&pkg, *force, &mut |f| eprintln!("writing {f}"))?;
            eprintln!("Installed \"{}\" (id {}). Undo with: ragekit oiv uninstall {} --game \"{}\"", rec.package_name, rec.id, rec.id, game.display());
            Ok(())
        }
        OivCmd::List { game } => {
            let game = game_dir(game)?;
            let list = oiv::list(&game);
            if list.is_empty() {
                println!("No packages installed by ragekit in {}", game.display());
            }
            for r in list {
                println!("{}  {} {}  ({} files, {})", r.id, r.package_name, r.package_version, r.files.len(), r.target);
            }
            Ok(())
        }
        OivCmd::Uninstall { id, game, force } => {
            let game = game_dir(game)?;
            for l in oiv::uninstall(&game, id, *force)? {
                eprintln!("{l}");
            }
            eprintln!("Uninstalled {id}");
            Ok(())
        }
    }
}
