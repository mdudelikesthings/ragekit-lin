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
    },
    /// Print one file to stdout (handy for .meta / .xml / .dat)
    Cat { archive: PathBuf, path: String },
    /// Build a new archive from a folder
    Pack {
        folder: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// v = GTA V RPF7 (default), iv = GTA IV RPF2, img = GTA IV IMG3.
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
    /// Search names across a game folder or archive, including inside archives.
    /// PATTERN is case-insensitive and may use * and ?
    Find { root: PathBuf, pattern: String },
}

#[derive(Subcommand)]
pub enum KeysCmd {
    /// Show which keys are installed
    Status,
    /// Find keys in your games and save them. Combine sources as needed.
    Extract {
        /// GTA5.exe / GTAIV.exe / EFLC.exe, or a game folder
        #[arg(long)]
        exe: Vec<PathBuf>,
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
        Cmd::Extract { archive, paths, out, expand_rpf } => {
            let t = open(archive, keys()?)?;
            fs::create_dir_all(out)?;
            let targets = if paths.is_empty() { vec![String::new()] } else { paths.clone() };
            let (mut n, mut failed) = (0, Vec::new());
            for p in targets {
                let p = normalize_path(&p);
                let dest = if p.is_empty() { out.clone() } else { out.join(p.rsplit('/').next().unwrap()) };
                let (c, f) = ops::extract(&t, &p, &dest, *expand_rpf, &mut |_| {})?;
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
        Cmd::Pack { folder, out, format } => {
            let fmt = match format.as_deref().map(str::to_ascii_lowercase).as_deref() {
                Some("v") | Some("gta5") | Some("rpf7") => Format::Rpf7,
                Some("iv") | Some("gta4") | Some("rpf2") => Format::Rpf2,
                Some("img") | Some("img3") => Format::Img3,
                Some(other) => bail!("unknown format '{other}' (use v, iv or img)"),
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
            Ok(())
        }
        KeysCmd::Extract { exe, pid, running, magic, from_dir } => {
            let mut keys = Keys::load_dir(dir).unwrap_or_default();
            if let Some(d) = from_dir {
                keys.merge(Keys::load_dir(d).with_context(|| format!("importing {}", d.display()))?);
                eprintln!("Imported keys from {}", d.display());
            }
            for e in exe {
                let exe = ops::resolve_exe(e)?;
                eprintln!("Searching {} ...", exe.display());
                report(keys.search_bytes(&fs::read(&exe)?));
            }
            let pid = match (pid, running) {
                (Some(p), _) => Some(*p),
                (None, true) => Some(ops::find_game_pid().ok_or_else(|| anyhow!("no running game found"))?),
                _ => None,
            };
            if let Some(p) = pid {
                eprintln!("Scanning memory of process {p} ...");
                report(keys.search_process(p)?);
            }
            if let Some(m) = magic {
                keys.load_codewalker_magic(&fs::read(m)?).context("decoding magic.dat")?;
                eprintln!("Decoded GTA V NG keys from {}", m.display());
            }
            keys.verify()?;
            keys.save_dir(dir)?;
            println!("Saved to {}", dir.display());
            println!("GTA V: AES {}  NG {}   GTA IV: {}", yes(keys.has_aes()), yes(keys.has_ng()), yes(keys.has_iv()));
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
