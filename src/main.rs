use clap::Parser;
use std::env;
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
    /// Returns the directory name for this bucket.
    fn dir_name(self) -> &'static str {
        match self {
            Bucket::LastWeek => "last-week",
            Bucket::CurrentMonth => "current-month",
            Bucket::LastMonths => "last-months",
            Bucket::OldStuff => "old-stuff",
        }
    }

    /// Returns an array of all bucket variants in order.
    ///
    /// Useful for iterating over all buckets.
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

/// Main entry point for the refile application.
///
/// This function:
/// 1. Parses command-line arguments
/// 2. Creates bucket directories (or prints them in dry-run mode)
/// 3. Collects all items to be processed
/// 4. Plans move actions for each item
/// 5. Executes the planned actions
///
/// # Errors
///
/// Returns an error if:
/// - The source directory cannot be read
/// - Bucket directories cannot be created
/// - File metadata cannot be accessed
/// - A file conflict occurs (in non-rename mode)
/// - File operations fail
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

/// Checks if a path is a protected directory that should not be moved.
///
/// Protected directories include:
/// - Root directory (`/`)
/// - User's home directory (detected via HOME env var)
/// - Top-level directories (direct children of root, e.g., `/tmp`, `/var`, `/usr`)
///
/// # Arguments
///
/// * `path` - The path to check
///
/// # Returns
///
/// `true` if the path is a protected directory
fn is_protected_directory(path: &Path) -> bool {
    // Canonicalize the path if possible for accurate comparison
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Check if it's root
    if canonical == Path::new("/") {
        return true;
    }

    // Check if it's the user's home directory
    if let Ok(home) = env::var("HOME") {
        let canonical_home =
            fs::canonicalize(home).unwrap_or_else(|e| PathBuf::from(e.to_string()));
        if canonical == canonical_home {
            return true;
        }
    }

    // Check if this is a top-level directory (direct child of root)
    if let Some(parent) = canonical.parent() {
        if parent == Path::new("/") {
            return true;
        }
    }

    false
}

/// Determines which bucket a file belongs to based on its age.
///
/// # Bucket Classifications
///
/// - 0-7 days: `Bucket::LastWeek`
/// - 8-28 days: `Bucket::CurrentMonth`
/// - 29-92 days: `Bucket::LastMonths`
/// - 93+ days: `Bucket::OldStuff`
///
/// # Arguments
///
/// * `age` - The duration since the file was last modified
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

/// Computes the base refile directory path within the target directory.
///
/// # Arguments
///
/// * `target_dir` - The target directory where refile structure will be created
///
/// # Returns
///
/// Path to `<target_dir>/refile`
fn refile_base_path(target_dir: &Path) -> PathBuf {
    target_dir.join("refile")
}

/// Computes the destination directory path for a specific bucket.
///
/// # Arguments
///
/// * `target_dir` - The target directory where refile structure exists
/// * `bucket` - The bucket variant to get the directory for
///
/// # Returns
///
/// Path to `<target_dir>/refile/<bucket_name>`
fn bucket_dest_dir(target_dir: &Path, bucket: Bucket) -> PathBuf {
    refile_base_path(target_dir).join(bucket.dir_name())
}

/// Computes the full destination path for a file based on its bucket.
///
/// # Arguments
///
/// * `source` - The source file path
/// * `target_dir` - The target directory where refile structure exists
/// * `bucket` - The bucket to place the file in
///
/// # Returns
///
/// `Some(PathBuf)` with the full destination path, or `None` if the source has no filename
fn compute_dest_path(source: &Path, target_dir: &Path, bucket: Bucket) -> Option<PathBuf> {
    let file_name = source.file_name()?;
    let dest_dir = bucket_dest_dir(target_dir, bucket);
    Some(dest_dir.join(file_name))
}

/// Generates a unique filename by appending a numeric suffix.
///
/// The suffix is inserted before the file extension, if present.
///
/// # Arguments
///
/// * `base` - The base path to generate a variant of
/// * `suffix` - The numeric suffix to append
///
/// # Returns
///
/// A new path with the suffix inserted: `filename (N).ext` or `filename (N)`
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

/// Checks if a path represents a bucket directory.
///
/// A valid bucket directory must:
/// 1. Have a parent directory named "refile"
/// 2. Have a name that matches one of the bucket names: "last-week",
///    "current-month", "last-months", or "old-stuff"
///
/// # Arguments
///
/// * `path` - The full path to check (can be &Path, &`PathBuf`, &str, etc.)
///
/// # Returns
///
/// `true` if the path is a valid bucket directory
fn is_bucket_dir<P: AsRef<Path>>(path: P) -> bool {
    let path = path.as_ref();

    // Check if parent directory is named "refile"
    let Some(parent) = path.parent() else {
        return false;
    };

    let Some(parent_name) = parent.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    if parent_name != "refile" {
        return false;
    }

    // Check if the directory name is a bucket name
    let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    matches!(
        dir_name,
        "last-week" | "current-month" | "last-months" | "old-stuff"
    )
}

/// Compares two paths for equality, attempting canonical comparison first.
///
/// This function first tries to canonicalize both paths (resolving symlinks and
/// relative components). If canonicalization fails for either path, it falls back
/// to direct path comparison.
///
/// **Note**: This function performs IO (via `fs::canonicalize`) and is not strictly pure.
///
/// # Arguments
///
/// * `a` - First path to compare
/// * `b` - Second path to compare
///
/// # Returns
///
/// `true` if the paths refer to the same location
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

/// Retrieves the age of a file based on its modification time.
///
/// The age is calculated as the duration between now and the file's last
/// modification time. Falls back to creation time if modification time is
/// unavailable, and to zero age if both are unavailable.
///
/// # Arguments
///
/// * `path` - Path to the file or directory
///
/// # Returns
///
/// `Ok(Duration)` representing the file's age, or an error if metadata cannot be read
///
/// # Errors
///
/// Returns an error if the file metadata cannot be accessed (e.g., file doesn't exist,
/// permission denied).
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

/// Finds a unique destination path by trying numbered suffixes.
///
/// If the base path doesn't exist, returns it unchanged. Otherwise, tries
/// appending (1), (2), (3), etc. until finding a path that doesn't exist.
///
/// # Arguments
///
/// * `base` - The base path to find a unique variant of
///
/// # Returns
///
/// `Ok(PathBuf)` with either the original path or a unique numbered variant
///
/// # Errors
///
/// Returns an error if no unique path can be found after trying 10,000 suffixes.
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

/// Creates the refile base directory and all bucket subdirectories.
///
/// This function ensures that the complete directory structure exists:
/// - `<refile_base>/`
/// - `<refile_base>/last-week/`
/// - `<refile_base>/current-month/`
/// - `<refile_base>/last-months/`
/// - `<refile_base>/old-stuff/`
///
/// # Arguments
///
/// * `refile_base` - Path to the refile base directory
///
/// # Errors
///
/// Returns an error if any directory cannot be created due to permissions or
/// other filesystem issues.
fn create_bucket_dirs(refile_base: &Path) -> io::Result<()> {
    fs::create_dir_all(refile_base)?;
    for bucket in Bucket::all() {
        fs::create_dir_all(refile_base.join(bucket.dir_name()))?;
    }
    Ok(())
}

/// Prints the directories that would be created in dry-run mode.
///
/// Checks which directories don't exist and prints what would be created,
/// without actually creating them.
///
/// # Arguments
///
/// * `refile_base` - Path to the refile base directory
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

/// Collects all items (files and directories) that need to be processed.
///
/// This function walks the source directory and:
/// - Collects all top-level items
/// - For the refile directory itself, collects items from inside bucket directories
/// - Treats stray items under refile/ as items to be processed
///
/// # Arguments
///
/// * `source_dir` - The directory to scan for items
/// * `refile_base` - Path to the refile base directory (for special handling)
///
/// # Returns
///
/// `Ok(Vec<PathBuf>)` containing all items to process
///
/// # Errors
///
/// Returns an error if the source directory cannot be read or if there are
/// issues reading subdirectories.
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
                    if is_bucket_dir(&p) {
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

/// Plans the appropriate action for a single file or directory.
///
/// This function:
/// 1. Checks if the path is a protected directory
/// 2. Reads the item's age from its metadata
/// 3. Determines the appropriate bucket
/// 4. Computes the destination path
/// 5. Checks for conflicts and handles them based on configuration
/// 6. Returns a `FileAction` describing what should be done
///
/// # Arguments
///
/// * `path` - Path to the item to plan an action for
/// * `cfg` - Configuration including target directory and conflict handling
///
/// # Returns
///
/// - `Ok(Some(FileAction::Move))` if the item should be moved
/// - `Ok(Some(FileAction::Skip))` if the item should be skipped (with reason)
/// - `Ok(None)` if the item is already in the correct location
///
/// # Errors
///
/// Returns an error if:
/// - The path is a protected directory (root or home)
/// - File metadata cannot be read
/// - A conflict exists and `allow_rename` is false
/// - No unique destination can be found when `allow_rename` is true
fn plan_action(path: &Path, cfg: &Config) -> io::Result<Option<FileAction>> {
    // Check if this is a protected directory
    if is_protected_directory(path) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Refusing to move protected directory: {}. \
                 Protected directories include: root (/), user home, and top-level directories (/tmp, /var, /usr, etc.).",
                path.display()
            ),
        ));
    }

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
    let Some(dest_path) = compute_dest_path(path, &cfg.target_dir, bucket) else {
        return Ok(Some(FileAction::Skip {
            path: path.to_path_buf(),
            reason: "no file name".to_string(),
        }));
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

/// Executes a planned file action.
///
/// For `FileAction::Skip`, prints a message to stderr.
/// For `FileAction::Move`, attempts to move the file:
/// - In dry-run mode, only prints what would be done
/// - Otherwise, attempts atomic rename first
/// - Falls back to copy+delete for cross-filesystem moves
///
/// # Arguments
///
/// * `action` - The action to execute
/// * `dry_run` - If true, only prints actions without performing them
///
/// # Errors
///
/// Returns an error if the file operation fails.
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
                Ok(()) => {
                    println!("Moved {} -> {}", from.display(), to.display());
                    Ok(())
                }
                Err(rename_err) => {
                    // Cross-filesystem move: copy then delete
                    move_cross_filesystem(&from, &to, &rename_err)
                }
            }
        }
    }
}

/// Moves a file or directory across filesystem boundaries.
///
/// This function is called as a fallback when `fs::rename` fails (typically
/// because source and destination are on different filesystems). It performs
/// a copy+delete operation:
/// - For directories: recursively copies all contents, then removes the source
/// - For files: copies the file, then removes the source
///
/// # Arguments
///
/// * `from` - Source path to move from
/// * `to` - Destination path to move to
/// * `rename_err` - The original rename error (used for error messages)
///
/// # Errors
///
/// Returns an error if:
/// - Copying fails
/// - Removing the source fails (after successful copy)
fn move_cross_filesystem(from: &Path, to: &Path, rename_err: &io::Error) -> io::Result<()> {
    if from.is_dir() {
        match copy_dir_recursive(from, to) {
            Ok(()) => {
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
            Ok(_bytes) => {
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

/// Recursively copies a directory and all its contents.
///
/// This function creates the destination directory if it doesn't exist,
/// then recursively copies all files and subdirectories from source to
/// destination. Used as part of cross-filesystem move operations.
///
/// # Arguments
///
/// * `src` - Source directory to copy from
/// * `dst` - Destination directory to copy to
///
/// # Errors
///
/// Returns an error if:
/// - The destination cannot be created
/// - Any file or directory cannot be read
/// - Any file cannot be copied
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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pick_bucket_last_week() {
        // 0 days
        assert_eq!(pick_bucket(Duration::from_secs(0)), Bucket::LastWeek);
        // 3 days
        assert_eq!(
            pick_bucket(Duration::from_secs(3 * 24 * 3600)),
            Bucket::LastWeek
        );
        // Exactly 7 days
        assert_eq!(
            pick_bucket(Duration::from_secs(7 * 24 * 3600)),
            Bucket::LastWeek
        );
    }

    #[test]
    fn test_pick_bucket_current_month() {
        // 8 days (just over a week)
        assert_eq!(
            pick_bucket(Duration::from_secs(8 * 24 * 3600)),
            Bucket::CurrentMonth
        );
        // 14 days
        assert_eq!(
            pick_bucket(Duration::from_secs(14 * 24 * 3600)),
            Bucket::CurrentMonth
        );
        // Exactly 28 days
        assert_eq!(
            pick_bucket(Duration::from_secs(28 * 24 * 3600)),
            Bucket::CurrentMonth
        );
    }

    #[test]
    fn test_pick_bucket_last_months() {
        // 29 days
        assert_eq!(
            pick_bucket(Duration::from_secs(29 * 24 * 3600)),
            Bucket::LastMonths
        );
        // 60 days
        assert_eq!(
            pick_bucket(Duration::from_secs(60 * 24 * 3600)),
            Bucket::LastMonths
        );
        // Exactly 92 days
        assert_eq!(
            pick_bucket(Duration::from_secs(92 * 24 * 3600)),
            Bucket::LastMonths
        );
    }

    #[test]
    fn test_pick_bucket_old_stuff() {
        // 93 days
        assert_eq!(
            pick_bucket(Duration::from_secs(93 * 24 * 3600)),
            Bucket::OldStuff
        );
        // 365 days
        assert_eq!(
            pick_bucket(Duration::from_secs(365 * 24 * 3600)),
            Bucket::OldStuff
        );
        // Very old
        assert_eq!(
            pick_bucket(Duration::from_secs(1000 * 24 * 3600)),
            Bucket::OldStuff
        );
    }

    #[test]
    fn test_refile_base_path() {
        let base = refile_base_path(Path::new("/home/user/documents"));
        assert_eq!(base, PathBuf::from("/home/user/documents/refile"));
    }

    #[test]
    fn test_bucket_dest_dir() {
        let target = Path::new("/home/user/documents");

        assert_eq!(
            bucket_dest_dir(target, Bucket::LastWeek),
            PathBuf::from("/home/user/documents/refile/last-week")
        );
        assert_eq!(
            bucket_dest_dir(target, Bucket::CurrentMonth),
            PathBuf::from("/home/user/documents/refile/current-month")
        );
        assert_eq!(
            bucket_dest_dir(target, Bucket::LastMonths),
            PathBuf::from("/home/user/documents/refile/last-months")
        );
        assert_eq!(
            bucket_dest_dir(target, Bucket::OldStuff),
            PathBuf::from("/home/user/documents/refile/old-stuff")
        );
    }

    #[test]
    fn test_compute_dest_path() {
        let source = Path::new("/home/user/documents/file.txt");
        let target = Path::new("/home/user/archive");

        let dest = compute_dest_path(source, target, Bucket::LastWeek);
        assert_eq!(
            dest,
            Some(PathBuf::from(
                "/home/user/archive/refile/last-week/file.txt"
            ))
        );

        let dest = compute_dest_path(source, target, Bucket::OldStuff);
        assert_eq!(
            dest,
            Some(PathBuf::from(
                "/home/user/archive/refile/old-stuff/file.txt"
            ))
        );
    }

    #[test]
    fn test_compute_dest_path_no_filename() {
        let dest = compute_dest_path(Path::new("/"), Path::new("/home/user/archive"), Bucket::LastWeek);
        assert_eq!(dest, None);
    }

    #[test]
    fn test_generate_unique_name_with_extension() {
        let base = Path::new("/home/user/documents/file.txt");

        assert_eq!(
            generate_unique_name(base, 1),
            PathBuf::from("/home/user/documents/file (1).txt")
        );
        assert_eq!(
            generate_unique_name(base, 2),
            PathBuf::from("/home/user/documents/file (2).txt")
        );
        assert_eq!(
            generate_unique_name(base, 42),
            PathBuf::from("/home/user/documents/file (42).txt")
        );
    }

    #[test]
    fn test_generate_unique_name_without_extension() {
        let base = Path::new("/home/user/documents/my-directory");

        assert_eq!(
            generate_unique_name(base, 1),
            PathBuf::from("/home/user/documents/my-directory (1)")
        );
        assert_eq!(
            generate_unique_name(base, 5),
            PathBuf::from("/home/user/documents/my-directory (5)")
        );
    }

    #[test]
    fn test_generate_unique_name_multiple_extensions() {
        let base = Path::new("/home/user/archive.tar.gz");

        // Should only use the last extension
        assert_eq!(
            generate_unique_name(base, 1),
            PathBuf::from("/home/user/archive.tar (1).gz")
        );
    }

    #[test]
    fn test_is_bucket_dir() {
        // Valid bucket directories - using string slices
        assert!(is_bucket_dir("/home/user/refile/last-week"));
        assert!(is_bucket_dir("/home/user/refile/current-month"));
        assert!(is_bucket_dir("/home/user/refile/last-months"));
        assert!(is_bucket_dir("/home/user/refile/old-stuff"));

        // Valid bucket directories - different paths
        assert!(is_bucket_dir("/var/archive/refile/last-week"));
        assert!(is_bucket_dir("/tmp/refile/old-stuff"));

        // Invalid - parent not named "refile"
        assert!(!is_bucket_dir("/home/user/documents/last-week"));
        assert!(!is_bucket_dir("/home/user/archive/current-month"));

        // Invalid - not a bucket name
        assert!(!is_bucket_dir("/home/user/refile/other-dir"));
        assert!(!is_bucket_dir("/home/user/refile/last-week-backup"));
        assert!(!is_bucket_dir("/home/user/refile/"));

        // Invalid - wrong case
        assert!(!is_bucket_dir("/home/user/refile/LastWeek"));

        // Invalid - no parent
        assert!(!is_bucket_dir("/"));
    }

    #[test]
    fn test_bucket_dir_name() {
        assert_eq!(Bucket::LastWeek.dir_name(), "last-week");
        assert_eq!(Bucket::CurrentMonth.dir_name(), "current-month");
        assert_eq!(Bucket::LastMonths.dir_name(), "last-months");
        assert_eq!(Bucket::OldStuff.dir_name(), "old-stuff");
    }

    #[test]
    fn test_bucket_all() {
        let all = Bucket::all();
        assert_eq!(all.len(), 4);
        assert_eq!(all[0], Bucket::LastWeek);
        assert_eq!(all[1], Bucket::CurrentMonth);
        assert_eq!(all[2], Bucket::LastMonths);
        assert_eq!(all[3], Bucket::OldStuff);
    }

    #[test]
    fn test_paths_equal_same_path() {
        let path = Path::new("/tmp/test.txt");
        assert!(paths_equal(path, path));
    }

    #[test]
    fn test_paths_equal_different_paths() {
        assert!(!paths_equal(Path::new("/tmp/test1.txt"), Path::new("/tmp/test2.txt")));
    }

    #[test]
    fn test_paths_equal_nonexistent() {
        // Should still compare correctly even if paths don't exist
        let path1 = Path::new("/nonexistent/path1");
        let path2 = Path::new("/nonexistent/path2");
        assert!(!paths_equal(path1, path2));

        let path3 = Path::new("/nonexistent/path1");
        assert!(paths_equal(path1, path3));
    }

    #[test]
    fn test_is_protected_directory_root() {
        // Root directory should be protected
        assert!(is_protected_directory(Path::new("/")));
    }

    #[test]
    fn test_is_protected_directory_home() {
        // Home directory should be protected if HOME is set
        if let Ok(home) = env::var("HOME") {
            assert!(is_protected_directory(Path::new(&home)));
        }
    }

    #[test]
    fn test_is_protected_directory_top_level() {
        // Top-level directories (direct children of root) should be protected
        assert!(is_protected_directory(Path::new("/tmp")));
        assert!(is_protected_directory(Path::new("/var")));
        assert!(is_protected_directory(Path::new("/usr")));
        assert!(is_protected_directory(Path::new("/etc")));
    }

    #[test]
    fn test_is_protected_directory_subdirs_not_protected() {
        // Subdirectories of top-level dirs should NOT be protected
        assert!(!is_protected_directory(Path::new("/tmp/random")));
        assert!(!is_protected_directory(Path::new("/var/log")));
        assert!(!is_protected_directory(Path::new("/usr/local")));
    }
}
