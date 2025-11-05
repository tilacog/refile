mod config;

use clap::Parser;
use config::{BucketConfig, BucketDef};
use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Organize files by age into categorized subdirectories
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Config {
    /// Source directory to scan for files and directories
    source_dir: PathBuf,

    /// Target directory where refile/* subdirectories will be created (defaults to `source_dir`)
    target_dir: Option<PathBuf>,

    /// Perform a dry-run without moving files
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Allow renaming files to avoid conflicts (default: abort on conflict)
    #[arg(short = 'r', long, default_value_t = false)]
    allow_rename: bool,

    /// Allow moving protected directories (root, home, top-level directories) - USE WITH EXTREME CAUTION
    #[arg(long, default_value_t = false)]
    allow_dangerous_directories: bool,

    /// Override base folder name (default: "refile")
    #[arg(long)]
    base_folder: Option<String>,

    /// Override bucket configuration (format: "name1=days1,name2=days2,name3=null")
    #[arg(long)]
    buckets: Option<String>,
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
    let target_dir = cfg.target_dir.as_ref().unwrap_or(&cfg.source_dir);

    // Warn about dangerous directories flag
    if cfg.allow_dangerous_directories {
        println!("WARNING: --allow-dangerous-directories is enabled!");
        println!("This allows moving protected directories including:");
        println!("  - Root directory (/)");
        println!("  - Your home directory");
        println!("  - Top-level system directories (/tmp, /var, /usr, etc.)");
        println!("This can cause SEVERE SYSTEM DAMAGE. Use with extreme caution!");
        println!();
    }

    // Load configuration file
    let config_file = config::load_config_file()?;

    // Resolve bucket configuration
    let bucket_config = config::resolve_bucket_config(
        &cfg.source_dir,
        config_file.as_ref(),
        cfg.base_folder.as_deref(),
        cfg.buckets.as_deref(),
    )?;

    let refile_base = refile_base_path(target_dir, &bucket_config);

    // Ensure destination directories exist
    if cfg.dry_run {
        print_dry_run_dirs(&refile_base, &bucket_config);
    } else {
        create_bucket_dirs(&refile_base, &bucket_config)?;
    }

    // Collect all items to process
    let items = collect_items_to_process(&cfg.source_dir, &refile_base, &bucket_config)?;

    // Plan actions for each item
    let actions: Vec<_> = items
        .into_iter()
        .filter_map(|path| plan_action(&path, target_dir, &cfg, &bucket_config).transpose())
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
    if let Some(parent) = canonical.parent()
        && parent == Path::new("/")
    {
        return true;
    }

    false
}

/// Determines which bucket a file belongs to based on its age.
///
/// Iterates through bucket definitions and returns the first bucket
/// whose `max_age_days` threshold is greater than or equal to the file's age.
///
/// # Arguments
///
/// * `age` - The duration since the file was last modified
/// * `bucket_config` - The bucket configuration to use
///
/// # Returns
///
/// A reference to the matching `BucketDef`, or the last bucket (catch-all) if none match.
fn pick_bucket(age: Duration, bucket_config: &BucketConfig) -> &BucketDef {
    let age_days = age.as_secs() / (24 * 3600);

    for bucket in &bucket_config.buckets {
        if let Some(max_days) = bucket.max_age_days {
            if age_days <= max_days {
                return bucket;
            }
        } else {
            // This is a catch-all bucket (None age)
            return bucket;
        }
    }

    // Should never reach here if validation passed (ensures catch-all exists)
    // Return last bucket as fallback
    &bucket_config.buckets[bucket_config.buckets.len() - 1]
}

/// Computes the base refile directory path within the target directory.
///
/// # Arguments
///
/// * `target_dir` - The target directory where refile structure will be created
/// * `bucket_config` - The bucket configuration (for base folder name)
///
/// # Returns
///
/// Path to `<target_dir>/<base_folder>`
fn refile_base_path(target_dir: &Path, bucket_config: &BucketConfig) -> PathBuf {
    target_dir.join(&bucket_config.base_folder)
}

/// Computes the destination directory path for a specific bucket.
///
/// # Arguments
///
/// * `target_dir` - The target directory where refile structure exists
/// * `bucket` - The bucket definition to get the directory for
/// * `bucket_config` - The bucket configuration (for base folder name)
///
/// # Returns
///
/// Path to `<target_dir>/<base_folder>/<bucket_name>`
fn bucket_dest_dir(target_dir: &Path, bucket: &BucketDef, bucket_config: &BucketConfig) -> PathBuf {
    refile_base_path(target_dir, bucket_config).join(&bucket.name)
}

/// Computes the full destination path for a file based on its bucket.
///
/// # Arguments
///
/// * `source` - The source file path
/// * `target_dir` - The target directory where refile structure exists
/// * `bucket` - The bucket to place the file in
/// * `bucket_config` - The bucket configuration (for base folder name)
///
/// # Returns
///
/// `Some(PathBuf)` with the full destination path, or `None` if the source has no filename
fn compute_dest_path(
    source: &Path,
    target_dir: &Path,
    bucket: &BucketDef,
    bucket_config: &BucketConfig,
) -> Option<PathBuf> {
    let file_name = source.file_name()?;
    let dest_dir = bucket_dest_dir(target_dir, bucket, bucket_config);
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
/// 1. Have a parent directory that matches the base folder name
/// 2. Have a name that matches one of the configured bucket names
///
/// # Arguments
///
/// * `path` - The full path to check (can be &Path, &`PathBuf`, &str, etc.)
/// * `bucket_config` - The bucket configuration to check against
///
/// # Returns
///
/// `true` if the path is a valid bucket directory
fn is_bucket_dir<P: AsRef<Path>>(path: P, bucket_config: &BucketConfig) -> bool {
    let path = path.as_ref();

    // Check if parent directory is named with the base folder name
    let Some(parent) = path.parent() else {
        return false;
    };

    let Some(parent_name) = parent.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    if parent_name != bucket_config.base_folder {
        return false;
    }

    // Check if the directory name is a bucket name
    let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    bucket_config
        .buckets
        .iter()
        .any(|bucket| bucket.name == dir_name)
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
        format!(
            "Cannot find a unique name for '{}' - files already exist with names up to '{} (10000)'.\n\
             \n\
             You're using --allow-rename (-r), but there are too many conflicting files.\n\
             Consider organizing the destination directory first or removing some duplicates.",
            base.display(),
            base.file_stem().and_then(|s| s.to_str()).unwrap_or("file")
        ),
    ))
}

/// Creates the refile base directory and all bucket subdirectories.
///
/// This function ensures that the complete directory structure exists based
/// on the bucket configuration.
///
/// # Arguments
///
/// * `refile_base` - Path to the refile base directory
/// * `bucket_config` - The bucket configuration to use
///
/// # Errors
///
/// Returns an error if any directory cannot be created due to permissions or
/// other filesystem issues.
fn create_bucket_dirs(refile_base: &Path, bucket_config: &BucketConfig) -> io::Result<()> {
    fs::create_dir_all(refile_base)?;
    for bucket in &bucket_config.buckets {
        fs::create_dir_all(refile_base.join(&bucket.name))?;
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
/// * `bucket_config` - The bucket configuration to use
fn print_dry_run_dirs(refile_base: &Path, bucket_config: &BucketConfig) {
    if !refile_base.exists() {
        println!("[dry-run] CREATE DIR {}", refile_base.display());
    }
    for bucket in &bucket_config.buckets {
        let dir = refile_base.join(&bucket.name);
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
/// * `bucket_config` - The bucket configuration to check bucket directories
///
/// # Returns
///
/// `Ok(Vec<PathBuf>)` containing all items to process
///
/// # Errors
///
/// Returns an error if the source directory cannot be read or if there are
/// issues reading subdirectories.
fn collect_items_to_process(
    source_dir: &Path,
    refile_base: &Path,
    bucket_config: &BucketConfig,
) -> io::Result<Vec<PathBuf>> {
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
                    if is_bucket_dir(&p, bucket_config) {
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
/// * `target_dir` - Target directory for refile structure
/// * `cfg` - Configuration including target directory and conflict handling
/// * `bucket_config` - The bucket configuration to use
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
/// - The path is a protected directory (root or home) and `allow_dangerous_directories` is false
/// - File metadata cannot be read
/// - A conflict exists and `allow_rename` is false
/// - No unique destination can be found when `allow_rename` is true
fn plan_action(
    path: &Path,
    target_dir: &Path,
    cfg: &Config,
    bucket_config: &BucketConfig,
) -> io::Result<Option<FileAction>> {
    // Check if this is a protected directory
    if is_protected_directory(path) && !cfg.allow_dangerous_directories {
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

    // Determine bucket
    let bucket = pick_bucket(age, bucket_config);

    // Compute destination path
    let Some(dest_path) = compute_dest_path(path, target_dir, bucket, bucket_config) else {
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

    fn default_config() -> BucketConfig {
        BucketConfig::default()
    }

    #[test]
    fn test_pick_bucket_with_default_config() {
        let config = default_config();

        // 0 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(0), &config);
        assert_eq!(bucket.name, "last-week");

        // 3 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(3 * 24 * 3600), &config);
        assert_eq!(bucket.name, "last-week");

        // 7 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(7 * 24 * 3600), &config);
        assert_eq!(bucket.name, "last-week");

        // 8 days -> current-month
        let bucket = pick_bucket(Duration::from_secs(8 * 24 * 3600), &config);
        assert_eq!(bucket.name, "current-month");

        // 28 days -> current-month
        let bucket = pick_bucket(Duration::from_secs(28 * 24 * 3600), &config);
        assert_eq!(bucket.name, "current-month");

        // 29 days -> last-months
        let bucket = pick_bucket(Duration::from_secs(29 * 24 * 3600), &config);
        assert_eq!(bucket.name, "last-months");

        // 92 days -> last-months
        let bucket = pick_bucket(Duration::from_secs(92 * 24 * 3600), &config);
        assert_eq!(bucket.name, "last-months");

        // 93 days -> old-stuff
        let bucket = pick_bucket(Duration::from_secs(93 * 24 * 3600), &config);
        assert_eq!(bucket.name, "old-stuff");

        // 365 days -> old-stuff
        let bucket = pick_bucket(Duration::from_secs(365 * 24 * 3600), &config);
        assert_eq!(bucket.name, "old-stuff");
    }

    #[test]
    fn test_pick_bucket_with_custom_config() {
        let config = BucketConfig {
            base_folder: "sorted".to_string(),
            buckets: vec![
                BucketDef {
                    name: "today".to_string(),
                    max_age_days: Some(1),
                },
                BucketDef {
                    name: "week".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "old".to_string(),
                    max_age_days: None,
                },
            ],
        };

        // 0 days -> today
        let bucket = pick_bucket(Duration::from_secs(0), &config);
        assert_eq!(bucket.name, "today");

        // 1 day -> today
        let bucket = pick_bucket(Duration::from_secs(24 * 3600), &config);
        assert_eq!(bucket.name, "today");

        // 2 days -> week
        let bucket = pick_bucket(Duration::from_secs(2 * 24 * 3600), &config);
        assert_eq!(bucket.name, "week");

        // 7 days -> week
        let bucket = pick_bucket(Duration::from_secs(7 * 24 * 3600), &config);
        assert_eq!(bucket.name, "week");

        // 8 days -> old
        let bucket = pick_bucket(Duration::from_secs(8 * 24 * 3600), &config);
        assert_eq!(bucket.name, "old");

        // 100 days -> old
        let bucket = pick_bucket(Duration::from_secs(100 * 24 * 3600), &config);
        assert_eq!(bucket.name, "old");
    }

    #[test]
    fn test_refile_base_path() {
        let config = default_config();
        let base = refile_base_path(Path::new("/home/user/documents"), &config);
        assert_eq!(base, PathBuf::from("/home/user/documents/refile"));

        let custom_config = BucketConfig {
            base_folder: "archive".to_string(),
            buckets: vec![BucketDef {
                name: "old".to_string(),
                max_age_days: None,
            }],
        };
        let base = refile_base_path(Path::new("/home/user/documents"), &custom_config);
        assert_eq!(base, PathBuf::from("/home/user/documents/archive"));
    }

    #[test]
    fn test_bucket_dest_dir() {
        let config = default_config();
        let target = Path::new("/home/user/documents");

        let bucket = &config.buckets[0]; // last-week
        assert_eq!(
            bucket_dest_dir(target, bucket, &config),
            PathBuf::from("/home/user/documents/refile/last-week")
        );

        let bucket = &config.buckets[1]; // current-month
        assert_eq!(
            bucket_dest_dir(target, bucket, &config),
            PathBuf::from("/home/user/documents/refile/current-month")
        );
    }

    #[test]
    fn test_compute_dest_path() {
        let config = default_config();
        let source = Path::new("/home/user/documents/file.txt");
        let target = Path::new("/home/user/archive");

        let bucket = &config.buckets[0]; // last-week
        let dest = compute_dest_path(source, target, bucket, &config);
        assert_eq!(
            dest,
            Some(PathBuf::from(
                "/home/user/archive/refile/last-week/file.txt"
            ))
        );

        let bucket = &config.buckets[3]; // old-stuff
        let dest = compute_dest_path(source, target, bucket, &config);
        assert_eq!(
            dest,
            Some(PathBuf::from(
                "/home/user/archive/refile/old-stuff/file.txt"
            ))
        );
    }

    #[test]
    fn test_compute_dest_path_no_filename() {
        let config = default_config();
        let bucket = &config.buckets[0];
        let dest = compute_dest_path(
            Path::new("/"),
            Path::new("/home/user/archive"),
            bucket,
            &config,
        );
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
        let config = default_config();

        // Valid bucket directories - using string slices
        assert!(is_bucket_dir("/home/user/refile/last-week", &config));
        assert!(is_bucket_dir("/home/user/refile/current-month", &config));
        assert!(is_bucket_dir("/home/user/refile/last-months", &config));
        assert!(is_bucket_dir("/home/user/refile/old-stuff", &config));

        // Valid bucket directories - different paths
        assert!(is_bucket_dir("/var/archive/refile/last-week", &config));
        assert!(is_bucket_dir("/tmp/refile/old-stuff", &config));

        // Invalid - parent not named "refile"
        assert!(!is_bucket_dir("/home/user/documents/last-week", &config));
        assert!(!is_bucket_dir("/home/user/archive/current-month", &config));

        // Invalid - not a bucket name
        assert!(!is_bucket_dir("/home/user/refile/other-dir", &config));
        assert!(!is_bucket_dir(
            "/home/user/refile/last-week-backup",
            &config
        ));
        assert!(!is_bucket_dir("/home/user/refile/", &config));

        // Invalid - wrong case
        assert!(!is_bucket_dir("/home/user/refile/LastWeek", &config));

        // Invalid - no parent
        assert!(!is_bucket_dir("/", &config));
    }

    #[test]
    fn test_is_bucket_dir_with_custom_config() {
        let config = BucketConfig {
            base_folder: "archive".to_string(),
            buckets: vec![
                BucketDef {
                    name: "recent".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "old".to_string(),
                    max_age_days: None,
                },
            ],
        };

        // Valid with custom base folder
        assert!(is_bucket_dir("/home/user/archive/recent", &config));
        assert!(is_bucket_dir("/home/user/archive/old", &config));

        // Invalid - wrong base folder
        assert!(!is_bucket_dir("/home/user/refile/recent", &config));

        // Invalid - not in bucket list
        assert!(!is_bucket_dir("/home/user/archive/last-week", &config));
    }

    #[test]
    fn test_paths_equal_same_path() {
        let path = Path::new("/tmp/test.txt");
        assert!(paths_equal(path, path));
    }

    #[test]
    fn test_paths_equal_different_paths() {
        assert!(!paths_equal(
            Path::new("/tmp/test1.txt"),
            Path::new("/tmp/test2.txt")
        ));
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

    #[test]
    fn test_plan_action_rejects_protected_dir_by_default() {
        // Test that protected directories are rejected when allow_dangerous_directories is false
        let cfg = Config {
            source_dir: PathBuf::from("/tmp"),
            target_dir: None,
            dry_run: false,
            allow_rename: false,
            allow_dangerous_directories: false,
            base_folder: None,
            buckets: None,
        };

        let bucket_config = default_config();
        let target = Path::new("/tmp");
        let protected_path = Path::new("/tmp"); // /tmp is a protected top-level directory

        // This should return an error because /tmp is protected and flag is false
        let result = plan_action(protected_path, target, &cfg, &bucket_config);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn test_plan_action_allows_protected_dir_with_flag() {
        // Test that protected directories are allowed when allow_dangerous_directories is true
        let cfg = Config {
            source_dir: PathBuf::from("/tmp"),
            target_dir: None,
            dry_run: false,
            allow_rename: false,
            allow_dangerous_directories: true,
            base_folder: None,
            buckets: None,
        };

        let bucket_config = default_config();
        let target = Path::new("/tmp");
        let protected_path = Path::new("/tmp");

        // This should NOT return a permission denied error because the flag is true
        // It may return other errors or succeed, but NOT PermissionDenied for protected dir
        let result = plan_action(protected_path, target, &cfg, &bucket_config);

        // If there's an error, it should not be PermissionDenied
        if let Err(e) = result {
            assert_ne!(
                e.kind(),
                io::ErrorKind::PermissionDenied,
                "Should not reject protected directory when allow_dangerous_directories is true"
            );
        }
    }

    #[test]
    fn test_plan_action_allows_nonprotected_dirs_regardless_of_flag() {
        // Test that non-protected directories work with both flag values
        let cfg_false = Config {
            source_dir: PathBuf::from("/tmp/test"),
            target_dir: None,
            dry_run: false,
            allow_rename: false,
            allow_dangerous_directories: false,
            base_folder: None,
            buckets: None,
        };

        let cfg_true = Config {
            source_dir: PathBuf::from("/tmp/test"),
            target_dir: None,
            dry_run: false,
            allow_rename: false,
            allow_dangerous_directories: true,
            base_folder: None,
            buckets: None,
        };

        let bucket_config = default_config();

        // Create a test path that is NOT protected
        let non_protected = Path::new("/tmp/test/some-dir");
        let target = Path::new("/tmp/test");

        // Both should NOT return PermissionDenied for protected directories
        // (they may fail for other reasons like file not found, but not for being protected)
        let result_false = plan_action(non_protected, target, &cfg_false, &bucket_config);
        let result_true = plan_action(non_protected, target, &cfg_true, &bucket_config);

        // Neither should fail with PermissionDenied for protected directory
        if let Err(e) = result_false {
            assert_ne!(
                e.kind(),
                io::ErrorKind::PermissionDenied,
                "Non-protected directory should not be rejected"
            );
        }

        if let Err(e) = result_true {
            assert_ne!(
                e.kind(),
                io::ErrorKind::PermissionDenied,
                "Non-protected directory should not be rejected"
            );
        }
    }
}
