use clap::Parser;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    fn all() -> [Bucket; 4] {
        [
            Bucket::LastWeek,
            Bucket::CurrentMonth,
            Bucket::LastMonths,
            Bucket::OldStuff,
        ]
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

    /// Allow renaming files to avoid conflicts (default: abort on conflict)
    #[arg(short = 'r', long, default_value_t = false)]
    allow_rename: bool,
}

#[derive(Debug)]
enum FileAction {
    Move { from: PathBuf, to: PathBuf },
    Skip { path: PathBuf, reason: String },
}

fn main() -> io::Result<()> {
    let cfg = Config::parse();

    let refile_base = refile_base_path(&cfg.target_dir);

    // Ensure destination directories exist
    if cfg.dry_run {
        print_dry_run_dirs(&refile_base);
    } else {
        create_bucket_dirs(&refile_base)?;
    }

    // Collect all items to process
    let items = collect_items_to_process(&cfg.source_dir, &refile_base)?;

    // Plan actions for each item
    let actions: Vec<_> = items
        .into_iter()
        .filter_map(|path| plan_action(&path, &cfg).transpose())
        .collect::<io::Result<_>>()?;

    // Execute actions
    for action in actions {
        execute_action(action, cfg.dry_run)?;
    }

    Ok(())
}

// ============================================================================
// Pure functions - no IO
// ============================================================================

/// Pure: Calculate which bucket an age belongs to
fn pick_bucket(age: Duration) -> Bucket {
    const WEEK: Duration = Duration::from_secs(7 * 24 * 3600);
    const MONTH: Duration = Duration::from_secs(28 * 24 * 3600);
    const THREE_MONTHS: Duration = Duration::from_secs(92 * 24 * 3600);

    if age <= WEEK {
        Bucket::LastWeek
    } else if age <= MONTH {
        Bucket::CurrentMonth
    } else if age <= THREE_MONTHS {
        Bucket::LastMonths
    } else {
        Bucket::OldStuff
    }
}

/// Pure: Compute the refile base path
fn refile_base_path(target_dir: &Path) -> PathBuf {
    target_dir.join("refile")
}

/// Pure: Compute destination directory for a bucket
fn bucket_dest_dir(target_dir: &Path, bucket: Bucket) -> PathBuf {
    refile_base_path(target_dir).join(bucket.dir_name())
}

/// Pure: Compute destination path for a file
fn compute_dest_path(source: &Path, target_dir: &Path, bucket: Bucket) -> Option<PathBuf> {
    let file_name = source.file_name()?;
    let dest_dir = bucket_dest_dir(target_dir, bucket);
    Some(dest_dir.join(file_name))
}

/// Pure: Generate a unique filename by appending a suffix
fn generate_unique_name(base: &Path, suffix: usize) -> PathBuf {
    let parent = base.parent().unwrap_or_else(|| Path::new("."));
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unnamed");
    let ext = base.extension().and_then(|e| e.to_str());

    if let Some(ext) = ext {
        parent.join(format!("{stem} ({suffix}).{ext}"))
    } else {
        parent.join(format!("{stem} ({suffix})"))
    }
}

/// Pure: Check if a path is a bucket directory
fn is_bucket_dir(name: &str) -> bool {
    matches!(
        name,
        "last-week" | "current-month" | "last-months" | "old-stuff"
    )
}

/// Pure: Check if paths are equal (best effort)
fn paths_equal(a: &Path, b: &Path) -> bool {
    // Try canonical comparison first
    if let (Ok(ca), Ok(cb)) = (fs::canonicalize(a), fs::canonicalize(b)) {
        return ca == cb;
    }
    // Fallback to direct comparison
    a == b
}

// ============================================================================
// IO functions - grouped together
// ============================================================================

/// IO: Get file age from metadata
fn get_file_age(path: &Path) -> io::Result<Duration> {
    let meta = fs::metadata(path)?;
    let modified = meta
        .modified()
        .or_else(|_| meta.created())
        .unwrap_or_else(|_| SystemTime::now());
    let now = SystemTime::now();
    let age = now
        .duration_since(modified)
        .unwrap_or(Duration::from_secs(0));
    Ok(age)
}

/// IO: Find a unique destination path that doesn't exist
fn find_unique_dest(base: &Path) -> io::Result<PathBuf> {
    if !base.exists() {
        return Ok(base.to_path_buf());
    }

    for i in 1..10_000 {
        let candidate = generate_unique_name(base, i);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "Could not find a unique destination name",
    ))
}

/// IO: Create all bucket directories
fn create_bucket_dirs(refile_base: &Path) -> io::Result<()> {
    fs::create_dir_all(refile_base)?;
    for bucket in Bucket::all() {
        fs::create_dir_all(refile_base.join(bucket.dir_name()))?;
    }
    Ok(())
}

/// IO: Print what directories would be created (dry-run)
fn print_dry_run_dirs(refile_base: &Path) {
    if !refile_base.exists() {
        println!("[dry-run] CREATE DIR {}", refile_base.display());
    }
    for bucket in Bucket::all() {
        let dir = refile_base.join(bucket.dir_name());
        if !dir.exists() {
            println!("[dry-run] CREATE DIR {}", dir.display());
        }
    }
}

/// IO: Collect all items that need to be processed
fn collect_items_to_process(source_dir: &Path, refile_base: &Path) -> io::Result<Vec<PathBuf>> {
    let mut items = Vec::new();

    let read_dir = fs::read_dir(source_dir).map_err(|e| {
        eprintln!(
            "Error reading source directory {}: {e}",
            source_dir.display()
        );
        e
    })?;

    for entry_res in read_dir {
        let entry = entry_res?;
        let path = entry.path();

        // Special handling for refile directory - look inside bucket dirs
        if path == refile_base {
            for child in fs::read_dir(refile_base)? {
                let child = child?;
                let p = child.path();

                if p.is_dir() {
                    let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");

                    if is_bucket_dir(name) {
                        // Process items inside bucket directories
                        for item in fs::read_dir(&p)? {
                            items.push(item?.path());
                        }
                    } else {
                        // Stray directory under refile/
                        items.push(p);
                    }
                } else {
                    // Stray file under refile/
                    items.push(p);
                }
            }
        } else {
            items.push(path);
        }
    }

    Ok(items)
}

/// IO: Plan an action for a single item (reads file metadata)
fn plan_action(path: &Path, cfg: &Config) -> io::Result<Option<FileAction>> {
    // Get file age
    let age = match get_file_age(path) {
        Ok(a) => a,
        Err(e) => {
            return Ok(Some(FileAction::Skip {
                path: path.to_path_buf(),
                reason: format!("cannot get age: {e}"),
            }));
        }
    };

    // Determine bucket (pure)
    let bucket = pick_bucket(age);

    // Compute destination path (pure)
    let dest_path = match compute_dest_path(path, &cfg.target_dir, bucket) {
        Some(p) => p,
        None => {
            return Ok(Some(FileAction::Skip {
                path: path.to_path_buf(),
                reason: "no file name".to_string(),
            }));
        }
    };

    // Check if source and destination are the same
    if paths_equal(path, &dest_path) {
        return Ok(None); // Skip silently - already in correct location
    }

    // Handle conflicts based on configuration
    let final_dest = if dest_path.exists() {
        if cfg.allow_rename {
            // Find a unique destination by renaming
            find_unique_dest(&dest_path)?
        } else {
            // Abort on conflict
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "Conflict: destination path already exists: {} (source: {})\n\
                     Use --allow-rename to automatically rename conflicting files",
                    dest_path.display(),
                    path.display()
                ),
            ));
        }
    } else {
        dest_path
    };

    Ok(Some(FileAction::Move {
        from: path.to_path_buf(),
        to: final_dest,
    }))
}

/// IO: Execute a planned action
fn execute_action(action: FileAction, dry_run: bool) -> io::Result<()> {
    match action {
        FileAction::Skip { path, reason } => {
            eprintln!("Skipping {}: {}", path.display(), reason);
            Ok(())
        }
        FileAction::Move { from, to } => {
            if dry_run {
                println!("[dry-run] MOVE {} -> {}", from.display(), to.display());
                return Ok(());
            }

            // Ensure parent directory exists
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)?;
            }

            // Try atomic rename first
            match fs::rename(&from, &to) {
                Ok(_) => {
                    println!("Moved {} -> {}", from.display(), to.display());
                    Ok(())
                }
                Err(rename_err) => {
                    // Cross-filesystem move: copy then delete
                    move_cross_filesystem(&from, &to, rename_err)
                }
            }
        }
    }
}

/// IO: Move file/directory across filesystems using copy+delete
fn move_cross_filesystem(from: &Path, to: &Path, rename_err: io::Error) -> io::Result<()> {
    if from.is_dir() {
        match copy_dir_recursive(from, to) {
            Ok(_) => {
                if let Err(e) = fs::remove_dir_all(from) {
                    eprintln!(
                        "Copied but failed to remove source dir {}: {e}",
                        from.display()
                    );
                    Err(e)
                } else {
                    println!("Moved {} -> {}", from.display(), to.display());
                    Ok(())
                }
            }
            Err(copy_err) => {
                eprintln!(
                    "Failed to move directory {} (rename: {}, copy: {})",
                    from.display(),
                    rename_err,
                    copy_err
                );
                Err(copy_err)
            }
        }
    } else {
        match fs::copy(from, to) {
            Ok(_) => {
                if let Err(e) = fs::remove_file(from) {
                    eprintln!(
                        "Copied but failed to remove source file {}: {e}",
                        from.display()
                    );
                    Err(e)
                } else {
                    println!("Moved {} -> {}", from.display(), to.display());
                    Ok(())
                }
            }
            Err(copy_err) => {
                eprintln!(
                    "Failed to move file {} (rename: {}, copy: {})",
                    from.display(),
                    rename_err,
                    copy_err
                );
                Err(copy_err)
            }
        }
    }
}

/// IO: Recursively copy a directory
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
