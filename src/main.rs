mod config;
mod core;
mod filesystem;

use clap::Parser;
use config::BucketConfig;
use core::{compute_dest_path, is_protected_directory, paths_equal, pick_bucket, refile_base_path};
use filesystem::{
    collect_items_to_process, create_bucket_dirs, find_unique_dest, get_file_age,
    move_cross_filesystem, print_dry_run_dirs,
};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

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
    let config_file = config::load_config_file()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    // Resolve bucket configuration
    let bucket_config = config::resolve_bucket_config(
        &cfg.source_dir,
        config_file.as_ref(),
        cfg.base_folder.as_deref(),
        cfg.buckets.as_deref(),
    )
    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

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
// Application logic
// ============================================================================

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

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::BucketDef;
    use crate::core::{
        bucket_dest_dir, compute_dest_path, generate_unique_name, is_bucket_dir,
        is_protected_directory, paths_equal, pick_bucket, refile_base_path,
    };
    use std::env;
    use std::time::Duration;

    fn default_config() -> BucketConfig {
        BucketConfig::default()
    }

    #[test]
    fn test_pick_bucket_with_default_config() {
        let config = default_config();

        // 0 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(0), &config);
        assert_eq!(bucket.name(), "last-week");

        // 3 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(3 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "last-week");

        // 7 days -> last-week
        let bucket = pick_bucket(Duration::from_secs(7 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "last-week");

        // 8 days -> current-month
        let bucket = pick_bucket(Duration::from_secs(8 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "current-month");

        // 28 days -> current-month
        let bucket = pick_bucket(Duration::from_secs(28 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "current-month");

        // 29 days -> last-months
        let bucket = pick_bucket(Duration::from_secs(29 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "last-months");

        // 92 days -> last-months
        let bucket = pick_bucket(Duration::from_secs(92 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "last-months");

        // 93 days -> old-stuff
        let bucket = pick_bucket(Duration::from_secs(93 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "old-stuff");

        // 365 days -> old-stuff
        let bucket = pick_bucket(Duration::from_secs(365 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "old-stuff");
    }

    #[test]
    fn test_pick_bucket_with_custom_config() {
        let config = BucketConfig::new_for_test(
            "sorted".to_string(),
            vec![
                BucketDef::new("today".to_string(), Some(1)),
                BucketDef::new("week".to_string(), Some(7)),
                BucketDef::new("old".to_string(), None),
            ],
        );

        // 0 days -> today
        let bucket = pick_bucket(Duration::from_secs(0), &config);
        assert_eq!(bucket.name(), "today");

        // 1 day -> today
        let bucket = pick_bucket(Duration::from_secs(24 * 3600), &config);
        assert_eq!(bucket.name(), "today");

        // 2 days -> week
        let bucket = pick_bucket(Duration::from_secs(2 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "week");

        // 7 days -> week
        let bucket = pick_bucket(Duration::from_secs(7 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "week");

        // 8 days -> old
        let bucket = pick_bucket(Duration::from_secs(8 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "old");

        // 100 days -> old
        let bucket = pick_bucket(Duration::from_secs(100 * 24 * 3600), &config);
        assert_eq!(bucket.name(), "old");
    }

    #[test]
    fn test_refile_base_path() {
        let config = default_config();
        let base = refile_base_path(Path::new("/home/user/documents"), &config);
        assert_eq!(base, PathBuf::from("/home/user/documents/refile"));

        let custom_config = BucketConfig::new_for_test(
            "archive".to_string(),
            vec![BucketDef::new("old".to_string(), None)],
        );
        let base = refile_base_path(Path::new("/home/user/documents"), &custom_config);
        assert_eq!(base, PathBuf::from("/home/user/documents/archive"));
    }

    #[test]
    fn test_bucket_dest_dir() {
        let config = default_config();
        let target = Path::new("/home/user/documents");

        let bucket = &config.buckets()[0]; // last-week
        assert_eq!(
            bucket_dest_dir(target, bucket, &config),
            PathBuf::from("/home/user/documents/refile/last-week")
        );

        let bucket = &config.buckets()[1]; // current-month
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

        let bucket = &config.buckets()[0]; // last-week
        let dest = compute_dest_path(source, target, bucket, &config);
        assert_eq!(
            dest,
            Some(PathBuf::from(
                "/home/user/archive/refile/last-week/file.txt"
            ))
        );

        let bucket = &config.buckets()[3]; // old-stuff
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
        let bucket = &config.buckets()[0];
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
        let config = BucketConfig::new_for_test(
            "archive".to_string(),
            vec![
                BucketDef::new("recent".to_string(), Some(7)),
                BucketDef::new("old".to_string(), None),
            ],
        );

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
        // Use root path which always exists
        let path = Path::new("/");
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
        // Nonexistent paths cannot be reliably compared, so should return false
        let path1 = Path::new("/nonexistent/path1");
        let path2 = Path::new("/nonexistent/path2");
        assert!(!paths_equal(path1, path2));

        // Even if the strings are identical, if they don't exist, we return false
        let path3 = Path::new("/nonexistent/path1");
        assert!(!paths_equal(path1, path3));
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
