use clap::Parser;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy)]
enum Bucket {
    LastWeek,
    CurrentMonth,
    LastMonths,
    OldStuff,
}

impl Bucket {
    fn dir_name(&self) -> &'static str {
        match self {
            Bucket::LastWeek => "last-week",
            Bucket::CurrentMonth => "current-month",
            Bucket::LastMonths => "last-months",
            Bucket::OldStuff => "old-stuff",
        }
    }
}

/// Organize files by age into categorized subdirectories
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Config {
    /// Source directory to scan for files and directories
    #[arg(short, long)]
    source_dir: PathBuf,

    /// Target directory where refile/* subdirectories will be created
    #[arg(short, long)]
    target_dir: PathBuf,

    /// Perform a dry-run without moving files
    #[arg(short = 'n', long)]
    dry_run: bool,
}

fn main() -> io::Result<()> {
    let cfg = Config::parse();

    // Ensure target base exists (dry-run prints instead)
    let refile_base = cfg.target_dir.join("refile");
    ensure_dir(&refile_base, cfg.dry_run)?;

    // Prepare destination subdirs
    for bucket in [
        Bucket::LastWeek,
        Bucket::CurrentMonth,
        Bucket::LastMonths,
        Bucket::OldStuff,
    ] {
        ensure_dir(&refile_base.join(bucket.dir_name()), cfg.dry_run)?;
    }

    // Iterate top-level entries of source_dir only (no recursion)
    let read_dir = match fs::read_dir(&cfg.source_dir) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!(
                "Error reading source directory {}: {e}",
                cfg.source_dir.display()
            );
            std::process::exit(1);
        }
    };

    for entry_res in read_dir {
        let entry = match entry_res {
            Ok(e) => e,
            Err(e) => {
                eprintln!("Error reading an entry: {e}");
                continue;
            }
        };

        let path = entry.path();

        // Skip moving the base "refile" dir itself as a unit; process its children instead.
        // But we still want to refile items within it. So: if the entry is exactly the "refile" dir,
        // we will iterate its immediate children and handle them individually.
        if path == refile_base {
            // Process children inside refile base, but still do not recurse into deeper subdirs.
            process_refile_base_children(&refile_base, &cfg)?;
            continue;
        }

        // For everything else in source, handle as a single unit (file or directory).
        handle_item(&path, &cfg)?;
    }

    Ok(())
}

fn process_refile_base_children(refile_base: &Path, cfg: &Config) -> io::Result<()> {
    // We want to refile items inside refile/* as they age.
    // Strategy:
    // - Iterate immediate children of refile_base (the bucket dirs), and also any stray items.
    // - For each bucket dir: iterate its immediate children and handle each as a unit.
    // - If there are non-bucket items directly under refile/, treat them as items to refile as well.

    let mut to_visit: Vec<PathBuf> = Vec::new();

    let rd = match fs::read_dir(refile_base) {
        Ok(rd) => rd,
        Err(e) => {
            eprintln!("Error reading {}: {e}", refile_base.display());
            return Ok(());
        }
    };

    for child in rd {
        let child = match child {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Error reading child of {}: {e}", refile_base.display());
                continue;
            }
        };
        let p = child.path();
        if p.is_dir() {
            // If it is one of our known bucket dirs, iterate its children.
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let is_bucket = matches!(
                name,
                "last-week" | "current-month" | "last-months" | "old-stuff"
            );
            if is_bucket {
                let inner = match fs::read_dir(&p) {
                    Ok(inner) => inner,
                    Err(e) => {
                        eprintln!("Error reading {}: {e}", p.display());
                        continue;
                    }
                };
                for item in inner {
                    match item {
                        Ok(i) => to_visit.push(i.path()),
                        Err(e) => eprintln!("Error reading subitem in {}: {e}", p.display()),
                    }
                }
            } else {
                // A stray directory under refile/; treat as an item to refile.
                to_visit.push(p);
            }
        } else {
            // A stray file under refile/; treat as an item to refile.
            to_visit.push(p);
        }
    }

    for p in to_visit {
        handle_item(&p, cfg)?;
    }

    Ok(())
}

fn handle_item(path: &Path, cfg: &Config) -> io::Result<()> {
    // Determine item age
    let age = match item_age(path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Skipping {}: cannot get age: {e}", path.display());
            return Ok(());
        }
    };

    let bucket = pick_bucket(age);

    // Compute destination path
    let dest_dir = cfg.target_dir.join("refile").join(bucket.dir_name());
    ensure_dir(&dest_dir, cfg.dry_run)?;

    let file_name = match path.file_name() {
        Some(n) => n.to_os_string(),
        None => {
            eprintln!("Skipping {}: no file name", path.display());
            return Ok(());
        }
    };

    let mut dest_path = dest_dir.join(&file_name);

    // If destination exists, generate a unique name to avoid overwrite.
    if dest_path.exists() {
        dest_path = unique_dest_path(&dest_path)?;
    }

    // If source and destination are the same path, skip.
    if same_file(path, &dest_path)? {
        // Nothing to do.
        return Ok(());
    }

    if cfg.dry_run {
        println!(
            "[dry-run] MOVE {} -> {}",
            path.display(),
            dest_path.display()
        );
        return Ok(());
    }

    // Ensure parent of destination exists
    if let Some(parent) = dest_path.parent() {
        fs::create_dir_all(parent)?;
    }

    // Attempt atomic rename first; if cross-device, fall back to copy+remove.
    match fs::rename(path, &dest_path) {
        Ok(_) => {
            println!("Moved {} -> {}", path.display(), dest_path.display());
        }
        Err(rename_err) => {
            // Try copy/remove approach
            if path.is_dir() {
                // Move directory as a unit (non-recursive copy via rename is preferred).
                // For cross-filesystem, use recursive copy then remove_dir_all.
                match copy_dir_recursive(path, &dest_path) {
                    Ok(_) => {
                        if let Err(e) = fs::remove_dir_all(path) {
                            eprintln!(
                                "Copied but failed to remove source dir {}: {e}",
                                path.display()
                            );
                        } else {
                            println!("Moved {} -> {}", path.display(), dest_path.display());
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "Failed to move directory {} (rename err: {}; copy err: {})",
                            path.display(),
                            rename_err,
                            e
                        );
                    }
                }
            } else {
                match fs::copy(path, &dest_path) {
                    Ok(_) => {
                        if let Err(e) = fs::remove_file(path) {
                            eprintln!(
                                "Copied but failed to remove source file {}: {e}",
                                path.display()
                            );
                        } else {
                            println!("Moved {} -> {}", path.display(), dest_path.display());
                        }
                    }
                    Err(copy_err) => {
                        eprintln!(
                            "Failed to move file {} (rename err: {}; copy err: {})",
                            path.display(),
                            rename_err,
                            copy_err
                        );
                    }
                }
            }
        }
    }

    Ok(())
}

fn item_age(path: &Path) -> io::Result<Duration> {
    let meta = fs::metadata(path)?;
    // Prefer modified time; if unavailable, fall back to created; else now.
    let modified = meta
        .modified()
        .or_else(|_| meta.created())
        .unwrap_or(SystemTime::now());
    let now = SystemTime::now();
    let age = now
        .duration_since(modified)
        .unwrap_or(Duration::from_secs(0));
    Ok(age)
}

fn pick_bucket(age: Duration) -> Bucket {
    // Thresholds:
    // 7 days, 28 days (4 weeks), 92 days (~3 months)
    let d1 = Duration::from_secs(7 * 24 * 3600);
    let d2 = Duration::from_secs(28 * 24 * 3600);
    let d3 = Duration::from_secs(92 * 24 * 3600);

    if age <= d1 {
        Bucket::LastWeek
    } else if age <= d2 {
        Bucket::CurrentMonth
    } else if age <= d3 {
        Bucket::LastMonths
    } else {
        Bucket::OldStuff
    }
}

fn ensure_dir(path: &Path, dry_run: bool) -> io::Result<()> {
    if dry_run {
        if !path.exists() {
            println!("[dry-run] CREATE DIR {}", path.display());
        }
        return Ok(());
    }
    if let Err(e) = fs::create_dir_all(path) {
        // Might be a file occupying the path; report error.
        if !path.exists() {
            return Err(e);
        }
    }
    Ok(())
}

// Create a unique destination path by appending a numeric suffix before extension.
fn unique_dest_path(base: &Path) -> io::Result<PathBuf> {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed");
    let ext = base.extension().and_then(|e| e.to_str());

    for i in 1..10_000 {
        let candidate = if let Some(ext) = ext {
            parent.join(format!("{stem} ({i}).{ext}"))
        } else {
            parent.join(format!("{stem} ({i})"))
        };
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not find a unique destination name",
    ))
}

// Best-effort same-file check based on canonical paths if possible; falls back to strict path equality.
fn same_file(a: &Path, b: &Path) -> io::Result<bool> {
    let ca = fs::canonicalize(a).unwrap_or_else(|_| a.to_path_buf());
    let cb = fs::canonicalize(b).unwrap_or_else(|_| b.to_path_buf());
    Ok(ca == cb)
}

// Recursive directory copy used only as fallback when rename fails across filesystems.
// Even though we conceptually treat directories as units (no scanning into them for classification),
// we still need recursion to actually copy their contents when moving across devices.
fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
    if !dst.exists() {
        fs::create_dir_all(dst)?;
    }
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &dest_path)?;
        } else {
            fs::copy(&path, &dest_path)?;
        }
    }
    Ok(())
}
