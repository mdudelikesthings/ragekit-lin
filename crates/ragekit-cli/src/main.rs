//! ragekit: a native Linux tool for GTA V archives (RPF7).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use ragekit_rpf::archive::EntryKind;
use ragekit_rpf::resource::describe_extension;
use ragekit_rpf::tree::node_from_disk;
use ragekit_rpf::{normalize_path, Archive, Encryption, Keys, Tree};

#[derive(Parser)]
#[command(name = "ragekit", version, about = "Browse, extract and build GTA V archives (RPF7) on Linux")]
struct Cli {
    /// Folder holding key files (default: ~/.config/ragekit/keys or $RAGEKIT_KEYS)
    #[arg(long, global = true)]
    keys: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage decryption keys taken from your own copy of the game
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
        /// Unpack nested .rpf archives into folders named like the archive
        #[arg(short = 'x', long)]
        expand_rpf: bool,
    },
    /// Print one file to stdout (handy for .meta / .xml)
    Cat { archive: PathBuf, path: String },
    /// Build a new OPEN archive from a folder (sub-folders named *.rpf become nested archives)
    Pack {
        folder: PathBuf,
        #[arg(short, long)]
        out: PathBuf,
        /// Table-of-contents encryption: open (default, mod loaders) or none
        #[arg(long, default_value = "open")]
        encryption: String,
    },
    /// Add or replace files/folders in an archive (rewritten as OPEN)
    Add {
        archive: PathBuf,
        /// Files or folders from disk
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
    /// Remove files/folders from an archive (rewritten as OPEN)
    Rm {
        archive: PathBuf,
        #[arg(required = true)]
        paths: Vec<String>,
        #[arg(short, long)]
        out: Option<PathBuf>,
        #[arg(long)]
        no_backup: bool,
    },
    /// Search file names across a game folder or archive, including nested archives.
    /// PATTERN is case-insensitive and may use * and ? wildcards.
    Find { root: PathBuf, pattern: String },
}

#[derive(Subcommand)]
enum KeysCmd {
    /// Show which keys are installed
    Status,
    /// Find keys in your game and save them. Combine sources as needed.
    Extract {
        /// GTA5.exe / GTA5_Enhanced.exe, or the game folder (finds the AES key)
        #[arg(long)]
        exe: Option<PathBuf>,
        /// Scan a running game process (finds all keys; needs ptrace rights)
        #[arg(long)]
        pid: Option<u32>,
        /// Find the running game automatically and scan it
        #[arg(long)]
        running: bool,
        /// CodeWalker's magic.dat (needs the AES key; supplies the NG keys)
        #[arg(long)]
        magic: Option<PathBuf>,
        /// Import key files from a CodeWalker/GTAUtil keys folder
        #[arg(long)]
        from_dir: Option<PathBuf>,
    },
}

fn keys_dir(cli: &Cli) -> PathBuf {
    if let Some(k) = &cli.keys {
        return k.clone();
    }
    if let Ok(k) = std::env::var("RAGEKIT_KEYS") {
        return PathBuf::from(k);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("ragekit").join("keys")
}

fn load_keys(cli: &Cli) -> Result<Arc<Keys>> {
    let dir = keys_dir(cli);
    let k = Keys::load_dir(&dir).with_context(|| format!("loading keys from {}", dir.display()))?;
    Ok(Arc::new(k))
}

fn open(path: &Path, keys: &Keys) -> Result<Archive> {
    Archive::open(path, keys).with_context(|| format!("opening {}", path.display()))
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(&cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    match &cli.cmd {
        Cmd::Keys(k) => keys_cmd(cli, k),
        Cmd::Info { archive } => {
            let keys = load_keys(cli)?;
            let a = open(archive, &keys)?;
            let (mut dirs, mut bins, mut res, mut rpfs) = (0, 0, 0, 0);
            let mut total: u64 = 0;
            for e in &a.entries {
                match e.kind {
                    EntryKind::Directory { .. } => dirs += 1,
                    EntryKind::Binary { .. } if e.is_rpf() => rpfs += 1,
                    EntryKind::Binary { .. } => bins += 1,
                    EntryKind::Resource { .. } => res += 1,
                }
                total += e.extracted_size();
            }
            println!("Archive:      {}", archive.display());
            println!("Size:         {}", human(a.data.len() as u64));
            println!("Encryption:   {}", a.encryption.label());
            println!("Folders:      {}", dirs.max(1) - 1);
            println!("Resources:    {res}");
            println!("Binary files: {bins}");
            println!("Nested RPFs:  {rpfs}");
            println!("Unpacked:     {}", human(total));
            Ok(())
        }
        Cmd::Ls { archive, path, recursive, long, nested } => {
            let keys = load_keys(cli)?;
            let top = open(archive, &keys)?;
            let (arc, idx) = enter(&top, path.as_deref().unwrap_or(""), &keys)?;
            let mut out = std::io::stdout().lock();
            if *recursive {
                list_recursive(&arc, idx, "", *long, *nested, &keys, &mut out)?;
            } else if arc.entries[idx].is_dir() {
                let mut kids = arc.children(idx);
                kids.sort_by(|a, b| arc.entries[*a].name.cmp(&arc.entries[*b].name));
                for c in kids {
                    print_entry(&arc, c, &arc.entries[c].name, *long, &mut out)?;
                }
            } else {
                print_entry(&arc, idx, &arc.entries[idx].name, *long, &mut out)?;
            }
            Ok(())
        }
        Cmd::Extract { archive, paths, out, expand_rpf } => {
            let keys = load_keys(cli)?;
            let top = open(archive, &keys)?;
            fs::create_dir_all(out)?;
            let mut count = 0usize;
            let mut failed = 0usize;
            let targets = if paths.is_empty() { vec![String::new()] } else { paths.clone() };
            for p in &targets {
                let (arc, idx) = top.resolve(p, &keys)?;
                let dest = if normalize_path(p).is_empty() { out.clone() } else { out.join(last_component(p)) };
                extract_entry(&arc, idx, &dest, *expand_rpf, &keys, &mut count, &mut failed)?;
            }
            eprintln!("Extracted {count} files to {}", out.display());
            if failed > 0 {
                bail!("{failed} files could not be extracted (see messages above)");
            }
            Ok(())
        }
        Cmd::Cat { archive, path } => {
            let keys = load_keys(cli)?;
            let top = open(archive, &keys)?;
            let (arc, idx) = top.resolve(path, &keys)?;
            let data = arc.extract(idx, &keys)?;
            std::io::stdout().lock().write_all(&data)?;
            Ok(())
        }
        Cmd::Pack { folder, out, encryption } => {
            let keys = load_keys(cli)?;
            let enc = match encryption.to_ascii_lowercase().as_str() {
                "open" => Encryption::Open,
                "none" => Encryption::None,
                other => bail!("unknown encryption '{other}' (use open or none)"),
            };
            if !folder.is_dir() {
                bail!("{} is not a folder", folder.display());
            }
            let tree = Tree::from_dir(folder, keys)?;
            let n = write_atomic(out, |w| Ok(tree.write(w, enc)?))?;
            eprintln!("Wrote {} ({})", out.display(), human(n));
            Ok(())
        }
        Cmd::Add { archive, sources, to, out, no_backup } => {
            let keys = load_keys(cli)?;
            let mut tree = Tree::from_archive(open(archive, &keys)?, keys.clone())?;
            let base = normalize_path(to);
            for s in sources {
                let name = s
                    .file_name()
                    .ok_or_else(|| anyhow!("bad source path {}", s.display()))?
                    .to_string_lossy()
                    .to_ascii_lowercase();
                let dest = if base.is_empty() { name } else { format!("{base}/{name}") };
                let node = node_from_disk(s, &keys).with_context(|| format!("reading {}", s.display()))?;
                let existed = tree.contains(&dest);
                tree.insert_node(&dest, node)?;
                eprintln!("{} {dest}", if existed { "replaced" } else { "added   " });
            }
            save_tree(&tree, archive, out.as_deref(), *no_backup)
        }
        Cmd::Rm { archive, paths, out, no_backup } => {
            let keys = load_keys(cli)?;
            let mut tree = Tree::from_archive(open(archive, &keys)?, keys.clone())?;
            for p in paths {
                tree.remove(p).with_context(|| format!("removing {p}"))?;
                eprintln!("removed {}", normalize_path(p));
            }
            save_tree(&tree, archive, out.as_deref(), *no_backup)
        }
        Cmd::Find { root, pattern } => {
            let keys = load_keys(cli)?;
            let pat = pattern.to_ascii_lowercase();
            let mut hits = 0usize;
            let mut out = std::io::stdout().lock();
            let rpfs: Vec<PathBuf> = if root.is_dir() { find_rpfs(root)? } else { vec![root.clone()] };
            for p in rpfs {
                let label = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().into_owned();
                let label = if label.is_empty() { p.to_string_lossy().into_owned() } else { label };
                match Archive::open(&p, &keys) {
                    Ok(a) => find_in(&a, &label, &pat, &keys, &mut out, &mut hits)?,
                    Err(e) => eprintln!("skipping {label}: {e}"),
                }
            }
            eprintln!("{hits} matches");
            Ok(())
        }
    }
}

fn keys_cmd(cli: &Cli, k: &KeysCmd) -> Result<()> {
    let dir = keys_dir(cli);
    match k {
        KeysCmd::Status => {
            let keys = Keys::load_dir(&dir)?;
            println!("Key folder: {}", dir.display());
            println!("AES key:          {}", yes(keys.aes.is_some()));
            println!("NG keys:          {}", yes(keys.ng_keys.is_some()));
            println!("NG tables:        {}", yes(keys.ng_tables.is_some()));
            println!("Hash lookup:      {}", yes(keys.hash_lut.is_some()));
            if !keys.has_ng() {
                println!("\nMost game archives are NG-encrypted. See `ragekit keys extract --help`.");
            }
            Ok(())
        }
        KeysCmd::Extract { exe, pid, running, magic, from_dir } => {
            let mut keys = Keys::load_dir(&dir).unwrap_or_default();
            if let Some(d) = from_dir {
                keys.merge(Keys::load_dir(d).with_context(|| format!("importing {}", d.display()))?);
                eprintln!("Imported keys from {}", d.display());
            }
            if let Some(e) = exe {
                let exe = resolve_exe(e)?;
                eprintln!("Searching {} ...", exe.display());
                let data = fs::read(&exe)?;
                report(keys.search_bytes(&data));
            }
            let pid = match (pid, running) {
                (Some(p), _) => Some(*p),
                (None, true) => Some(find_game_pid().ok_or_else(|| anyhow!("no running GTA5 process found"))?),
                _ => None,
            };
            if let Some(p) = pid {
                eprintln!("Scanning memory of process {p} ...");
                report(keys.search_process(p)?);
            }
            if let Some(m) = magic {
                keys.load_codewalker_magic(&fs::read(m)?).context("decoding magic.dat")?;
                eprintln!("Decoded NG keys from {}", m.display());
            }
            keys.verify()?;
            keys.save_dir(&dir)?;
            println!("Saved to {}", dir.display());
            println!("AES: {}   NG: {}", yes(keys.has_aes()), yes(keys.has_ng()));
            if !keys.has_ng() {
                println!(
                    "NG keys are still missing. Start the game (Steam/Proton is fine) and run\n  \
                     sudo ragekit --keys {} keys extract --running\nor use --magic with CodeWalker's magic.dat.",
                    dir.display()
                );
            }
            Ok(())
        }
    }
}

fn report(r: ragekit_rpf::keys::SearchReport) {
    eprintln!(
        "  AES key: {}  hash table: {}  NG keys: {}/101  NG tables: {}/272",
        yes(r.aes),
        yes(r.hash_lut),
        r.ng_keys_found,
        r.ng_tables_found
    );
}

fn yes(b: bool) -> &'static str {
    if b {
        "yes"
    } else {
        "no"
    }
}

fn resolve_exe(p: &Path) -> Result<PathBuf> {
    if p.is_file() {
        return Ok(p.to_path_buf());
    }
    for name in ["GTA5.exe", "GTA5_Enhanced.exe", "gta5.exe", "gta5_enhanced.exe", "PlayGTAV.exe"] {
        let c = p.join(name);
        if c.is_file() {
            return Ok(c);
        }
    }
    bail!("no GTA5.exe found in {}", p.display())
}

fn find_game_pid() -> Option<u32> {
    for e in fs::read_dir("/proc").ok()?.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else { continue };
        let cmd = fs::read(e.path().join("cmdline")).unwrap_or_default();
        let cmd = String::from_utf8_lossy(&cmd).to_ascii_lowercase();
        let exe = cmd.split('\0').next().unwrap_or("");
        if exe.ends_with("gta5.exe") || exe.ends_with("gta5_enhanced.exe") {
            return Some(pid);
        }
    }
    None
}

/// Resolve a path (possibly through nested archives) to (archive, entry).
/// A path naming a nested .rpf enters it (returning its root).
fn enter(top: &Archive, path: &str, keys: &Keys) -> Result<(Archive, usize)> {
    let (arc, idx) = top.resolve(path, keys)?;
    if arc.entries[idx].is_rpf() {
        let inner = arc.open_nested(idx, keys)?;
        return Ok((inner, 0));
    }
    Ok((arc, idx))
}

fn last_component(p: &str) -> String {
    normalize_path(p).rsplit('/').next().unwrap_or("").to_string()
}

fn print_entry(arc: &Archive, idx: usize, label: &str, long: bool, out: &mut impl Write) -> Result<()> {
    let e = &arc.entries[idx];
    let shown = if e.is_dir() { format!("{label}/") } else { label.to_string() };
    if !long {
        writeln!(out, "{shown}")?;
        return Ok(());
    }
    let (kind, size) = match e.kind {
        EntryKind::Directory { count, .. } => ("dir", format!("{count} items")),
        EntryKind::Binary { encrypted, .. } if e.is_rpf() => (if encrypted { "rpf*" } else { "rpf" }, human(e.extracted_size())),
        EntryKind::Binary { encrypted, .. } => (if encrypted { "bin*" } else { "bin" }, human(e.extracted_size())),
        EntryKind::Resource { system_flags, graphics_flags, .. } => {
            let _ = (system_flags, graphics_flags);
            ("rsc", human(e.extracted_size()))
        }
    };
    let desc = if e.is_dir() { "" } else { describe_extension(&e.name).unwrap_or("") };
    writeln!(out, "{kind:<5} {size:>10}  {shown}  {desc}")?;
    Ok(())
}

fn list_recursive(
    arc: &Archive,
    dir: usize,
    prefix: &str,
    long: bool,
    nested: bool,
    keys: &Keys,
    out: &mut impl Write,
) -> Result<()> {
    let mut kids = arc.children(dir);
    kids.sort_by(|a, b| arc.entries[*a].name.cmp(&arc.entries[*b].name));
    for c in kids {
        let e = &arc.entries[c];
        let label = if prefix.is_empty() { e.name.clone() } else { format!("{prefix}/{}", e.name) };
        print_entry(arc, c, &label, long, out)?;
        if e.is_dir() {
            list_recursive(arc, c, &label, long, nested, keys, out)?;
        } else if nested && e.is_rpf() {
            match arc.open_nested(c, keys) {
                Ok(inner) => list_recursive(&inner, 0, &label, long, nested, keys, out)?,
                Err(err) => eprintln!("{label}: {err}"),
            }
        }
    }
    Ok(())
}

fn extract_entry(
    arc: &Archive,
    idx: usize,
    dest: &Path,
    expand: bool,
    keys: &Keys,
    count: &mut usize,
    failed: &mut usize,
) -> Result<()> {
    let e = &arc.entries[idx];
    if e.is_dir() {
        fs::create_dir_all(dest)?;
        for c in arc.children(idx) {
            let name = sanitize(&arc.entries[c].name)?;
            extract_entry(arc, c, &dest.join(name), expand, keys, count, failed)?;
        }
        return Ok(());
    }
    if expand && e.is_rpf() {
        match arc.open_nested(idx, keys) {
            Ok(inner) => return extract_entry(&inner, 0, dest, expand, keys, count, failed),
            Err(err) => {
                eprintln!("{}: can't open nested archive ({err}); saving it as a file", arc.path_of(idx));
            }
        }
    }
    match arc.extract(idx, keys) {
        Ok(data) => {
            if let Some(p) = dest.parent() {
                fs::create_dir_all(p)?;
            }
            fs::write(dest, data).with_context(|| format!("writing {}", dest.display()))?;
            *count += 1;
        }
        Err(err) => {
            eprintln!("{}: {err}", arc.path_of(idx));
            *failed += 1;
        }
    }
    Ok(())
}

/// Refuse names that would escape the output folder.
fn sanitize(name: &str) -> Result<&str> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        bail!("refusing unsafe entry name {name:?}");
    }
    Ok(name)
}

fn find_rpfs(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rpf")) {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn find_in(arc: &Archive, label: &str, pat: &str, keys: &Keys, out: &mut impl Write, hits: &mut usize) -> Result<()> {
    for i in arc.walk() {
        if i == 0 {
            continue;
        }
        let e = &arc.entries[i];
        let full = format!("{label}/{}", arc.path_of(i));
        if wildcard(pat, &e.name.to_ascii_lowercase()) {
            writeln!(out, "{full}")?;
            *hits += 1;
        }
        if e.is_rpf() {
            match arc.open_nested(i, keys) {
                Ok(inner) => find_in(&inner, &full, pat, keys, out, hits)?,
                Err(err) => eprintln!("skipping {full}: {err}"),
            }
        }
    }
    Ok(())
}

/// Case-insensitive wildcard match; a pattern without wildcards matches substrings.
fn wildcard(pat: &str, s: &str) -> bool {
    if !pat.contains('*') && !pat.contains('?') {
        return s.contains(pat);
    }
    let (p, t): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    let (mut pi, mut ti, mut star, mut mark) = (0, 0, None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn save_tree(tree: &Tree, archive: &Path, out: Option<&Path>, no_backup: bool) -> Result<()> {
    let target = out.unwrap_or(archive);
    if out.is_none() && !no_backup {
        let bak = backup_path(archive);
        if !bak.exists() {
            fs::copy(archive, &bak).with_context(|| format!("backing up to {}", bak.display()))?;
            eprintln!("Backup: {}", bak.display());
        }
    }
    let n = write_atomic(target, |w| Ok(tree.write(w, Encryption::Open)?))?;
    eprintln!("Wrote {} ({}, OPEN)", target.display(), human(n));
    Ok(())
}

fn backup_path(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

/// Write via a temp file in the same folder, then rename over the target.
/// The source archive stays mapped and valid until we're done.
fn write_atomic(target: &Path, f: impl FnOnce(&mut std::io::BufWriter<fs::File>) -> Result<u64>) -> Result<u64> {
    let dir = target.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.ragekit-tmp-{}",
        target.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
        std::process::id()
    ));
    let res = (|| {
        let mut w = std::io::BufWriter::new(fs::File::create(&tmp)?);
        let n = f(&mut w)?;
        w.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        Ok::<u64, anyhow::Error>(n)
    })();
    match res {
        Ok(n) => {
            fs::rename(&tmp, target).with_context(|| format!("replacing {}", target.display()))?;
            Ok(n)
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn wildcards() {
        assert!(wildcard("adder", "adder.yft"));
        assert!(wildcard("*.ytd", "adder.ytd"));
        assert!(!wildcard("*.ytd", "adder.yft"));
        assert!(wildcard("add?r*", "adder_hi.yft"));
    }
}
