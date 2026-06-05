//! Pure business logic functions that perform no I/O operations.
//!
//! This module contains all the core logic for determining file organization,
//! computing paths, and other operations that don't interact with the filesystem.
//! These functions are easier to test and reason about since they have no side effects.

use crate::config::{Boundary, BucketConfig, BucketDef};
use chrono::{DateTime, Datelike, Days, NaiveDate, TimeZone, Utc};
use std::fs;
use std::path::{Path, PathBuf};

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
pub fn is_protected_directory(path: &Path) -> bool {
    // Canonicalize the path if possible for accurate comparison
    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());

    // Check if it's root
    if canonical == Path::new("/") {
        return true;
    }

    // Check if it's the user's home directory
    if let Ok(home) = std::env::var("HOME") {
        // If canonicalization fails for HOME, fall back to the raw path
        let canonical_home = fs::canonicalize(&home).unwrap_or_else(|_| PathBuf::from(home));
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

/// Determines which bucket a file belongs to based on its modification time.
///
/// Buckets are evaluated in declaration order. Each non-catch-all bucket has a
/// calendar *start instant* computed relative to `now`; the first bucket whose
/// start instant is at or before `mtime` wins. The catch-all bucket matches
/// anything older. Because the newest periods are listed first, this naturally
/// resolves the overlaps that occur when, for example, the current week spans a
/// month boundary.
///
/// # Arguments
///
/// * `mtime` - The file's modification time (UTC)
/// * `now` - The reference instant the calendar boundaries are computed from (UTC)
/// * `bucket_config` - The bucket configuration to use
///
/// # Returns
///
/// A reference to the matching `BucketDef`, or the last bucket (catch-all) if none match.
#[must_use]
pub fn pick_bucket(
    mtime: DateTime<Utc>,
    now: DateTime<Utc>,
    bucket_config: &BucketConfig,
) -> &BucketDef {
    for bucket in bucket_config.buckets() {
        match bucket_start(bucket.boundary(), now) {
            // Catch-all: matches everything older than the preceding buckets.
            None => return bucket,
            Some(start) if mtime >= start => return bucket,
            Some(_) => {}
        }
    }

    // Should never reach here if validation passed (ensures catch-all exists).
    // Return last bucket as fallback.
    &bucket_config.buckets()[bucket_config.buckets().len() - 1]
}

/// Computes the start instant (inclusive) for a bucket's calendar period,
/// relative to `now`. Returns `None` for the catch-all, which has no lower bound.
fn bucket_start(boundary: &Boundary, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let today = now.date_naive();
    let date = match boundary {
        Boundary::CurrentWeek => start_of_week(today),
        Boundary::LastWeek => start_of_week(today) - Days::new(7),
        Boundary::CurrentMonth => first_of_month(today),
        Boundary::LastMonth => first_of_previous_month(today),
        Boundary::CatchAll => return None,
    };
    Some(start_of_day_utc(date))
}

/// Returns the most recent Sunday on or before `date` (week starts on Sunday).
fn start_of_week(date: NaiveDate) -> NaiveDate {
    let days_since_sunday = date.weekday().num_days_from_sunday();
    date - Days::new(u64::from(days_since_sunday))
}

/// Returns the first day of `date`'s month.
fn first_of_month(date: NaiveDate) -> NaiveDate {
    date.with_day(1).expect("day 1 is valid for any month")
}

/// Returns the first day of the month preceding `date`'s month.
fn first_of_previous_month(date: NaiveDate) -> NaiveDate {
    let last_day_of_previous = first_of_month(date) - Days::new(1);
    first_of_month(last_day_of_previous)
}

/// Converts a date to the instant at its start (00:00:00 UTC).
fn start_of_day_utc(date: NaiveDate) -> DateTime<Utc> {
    Utc.from_utc_datetime(
        &date
            .and_hms_opt(0, 0, 0)
            .expect("midnight is valid for any date"),
    )
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
#[must_use]
pub fn refile_base_path(target_dir: &Path, bucket_config: &BucketConfig) -> PathBuf {
    target_dir.join(bucket_config.base_folder())
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
#[must_use]
pub fn bucket_dest_dir(
    target_dir: &Path,
    bucket: &BucketDef,
    bucket_config: &BucketConfig,
) -> PathBuf {
    refile_base_path(target_dir, bucket_config).join(bucket.name())
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
#[must_use]
pub fn compute_dest_path(
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
#[must_use]
pub fn generate_unique_name(base: &Path, suffix: usize) -> PathBuf {
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
pub fn is_bucket_dir<P: AsRef<Path>>(path: P, bucket_config: &BucketConfig) -> bool {
    let path = path.as_ref();

    // Check if parent directory is named with the base folder name
    let Some(parent) = path.parent() else {
        return false;
    };

    let Some(parent_name) = parent.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    if parent_name != bucket_config.base_folder() {
        return false;
    }

    // Check if the directory name is a bucket name
    let Some(dir_name) = path.file_name().and_then(|s| s.to_str()) else {
        return false;
    };

    bucket_config
        .buckets()
        .iter()
        .any(|bucket| bucket.name() == dir_name)
}

/// Compares two paths for equality, attempting canonical comparison.
///
/// This function tries to canonicalize both paths (resolving symlinks and
/// relative components). Only returns true if both paths can be canonicalized
/// and they refer to the same location.
///
/// If either path cannot be canonicalized (e.g., doesn't exist), returns false,
/// since we cannot reliably determine if they would refer to the same location.
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
/// `true` if both paths exist and refer to the same location, `false` otherwise
pub fn paths_equal(a: &Path, b: &Path) -> bool {
    // Only consider paths equal if BOTH can be canonicalized and match
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false, // If either fails, assume they're different
    }
}
