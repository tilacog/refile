//! File system operations and I/O functions.
//!
//! This module contains all functions that interact with the filesystem,
//! including reading file metadata, moving files, and directory operations.

use crate::config::BucketConfig;
use crate::core::{generate_unique_name, is_bucket_dir};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Retrieves the age of a file based on its modification time.
///
/// The age is calculated as the duration between now and the file's last
/// modification time. Falls back to creation time if modification time is
/// unavailable.
///
/// # Arguments
///
/// * `path` - Path to the file or directory
///
/// # Returns
///
/// `Ok(Duration)` representing the file's age, or an error if:
/// - File metadata cannot be accessed
/// - Neither modification nor creation time is available
/// - File timestamp is in the future (possible clock skew)
///
/// # Errors
///
/// Returns an error if the file metadata cannot be accessed (e.g., file doesn't exist,
/// permission denied), or if file timestamps are unavailable or invalid.
pub fn get_file_age(path: &Path) -> io::Result<Duration> {
    let meta = fs::metadata(path)?;

    // Try modification time first, fall back to creation time
    let timestamp = meta
        .modified()
        .or_else(|_| meta.created())
        .map_err(|e| io::Error::other(format!("Cannot read file timestamp: {e}")))?;

    let now = SystemTime::now();
    now.duration_since(timestamp)
        .map_err(|_| io::Error::other("File timestamp is in the future - check system clock"))
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
pub fn find_unique_dest(base: &Path) -> io::Result<PathBuf> {
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
pub fn create_bucket_dirs(refile_base: &Path, bucket_config: &BucketConfig) -> io::Result<()> {
    fs::create_dir_all(refile_base)?;
    for bucket in bucket_config.buckets() {
        fs::create_dir_all(refile_base.join(bucket.name()))?;
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
pub fn print_dry_run_dirs(refile_base: &Path, bucket_config: &BucketConfig) {
    if !refile_base.exists() {
        println!("[dry-run] CREATE DIR {}", refile_base.display());
    }
    for bucket in bucket_config.buckets() {
        let dir = refile_base.join(bucket.name());
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
pub fn collect_items_to_process(
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
pub fn move_cross_filesystem(from: &Path, to: &Path, rename_err: &io::Error) -> io::Result<()> {
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
pub fn copy_dir_recursive(src: &Path, dst: &Path) -> io::Result<()> {
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
