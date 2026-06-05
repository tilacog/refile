//! Integration tests for the refile CLI application.
//!
//! # Test Philosophy
//!
//! These integration tests verify end-to-end behavior through the CLI interface to ensure:
//!
//! 1. **Correctness**: Files are categorized and moved to correct calendar buckets
//! 2. **Safety**: Dry-run mode never modifies the filesystem
//! 3. **Conflict handling**: Rename logic prevents data loss when conflicts occur
//! 4. **Idempotency**: Repeated refiling produces correct results as files age
//! 5. **Configuration flexibility**: Custom buckets and folders work correctly
//!
//! # Time determinism
//!
//! Bucketing is calendar-aware (current week, last week, current month, …), so the
//! bucket a file lands in depends on *when* the tool runs. Tests stay deterministic
//! in one of two ways:
//!
//! - Pin the reference instant with the `REFILE_NOW` env var and give files absolute
//!   modification times (see [`create_file_on`]). Used by the comprehensive test.
//! - Use ages that always map to the same bucket regardless of the date: age 0 is
//!   always `current-week`; an age older than two months is always `old-stuff`.
//!
//! Each test represents a real user scenario and documents expected behavior.
//! Tests use temporary directories to ensure isolation and avoid side effects.

use assert_cmd::Command;
use assert_fs::TempDir;
use assert_fs::prelude::*;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

// Time constants
const SECONDS_PER_DAY: u64 = 24 * 3600;

// An age (in days) that always lands in current-week regardless of the date.
const RECENT_FILE_AGE: u64 = 0;
// An age (in days) that always lands in old-stuff regardless of the date: older
// than the first day of the previous month.
const OLD_FILE_AGE: u64 = 200;

// A pinned reference instant for deterministic calendar bucketing.
// Wednesday 2026-06-17; the week starts Sunday 2026-06-14.
const PINNED_NOW: &str = "2026-06-17T12:00:00Z";

// Bucket path constants
const REFILE_BASE: &str = "refile";
const CURRENT_WEEK_BUCKET: &str = "refile/current-week";
const LAST_WEEK_BUCKET: &str = "refile/last-week";
const CURRENT_MONTH_BUCKET: &str = "refile/current-month";
const LAST_MONTH_BUCKET: &str = "refile/last-month";
const OLD_STUFF_BUCKET: &str = "refile/old-stuff";

/// Helper to create a file with a specific age (days old).
fn create_file_with_age(dir: &Path, name: &str, days_old: u64) -> std::io::Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, b"test content")?;

    // Set the modification time to make the file appear older
    let age = SystemTime::now() - Duration::from_secs(days_old * SECONDS_PER_DAY);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(age))?;

    Ok(())
}

/// Helper to create a file whose modification time is a specific absolute UTC instant.
///
/// Pairs with the `REFILE_NOW` env var to make calendar bucketing deterministic.
fn create_file_on(dir: &Path, name: &str, rfc3339: &str) -> std::io::Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, b"test content")?;

    let mtime = chrono::DateTime::parse_from_rfc3339(rfc3339)
        .expect("valid RFC 3339 timestamp")
        .with_timezone(&chrono::Utc);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(mtime.into()))?;

    Ok(())
}

/// Helper to create a refile command
#[must_use]
fn refile_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_refile"))
}

/// Tests that running with no arguments fails with a helpful message rather than a panic.
#[test]
fn test_no_arguments_fails_gracefully() {
    refile_cmd()
        .assert()
        .failure()
        .stderr(predicates::str::contains("source_dir"));
}

/// Tests that `config dump` prints the example configuration and exits successfully.
///
/// Guards against the subcommand dispatch regression where a required positional made
/// every `config` subcommand fail with "`source_dir` required".
#[test]
fn test_config_dump_succeeds() {
    refile_cmd()
        .args(["config", "dump"])
        .assert()
        .success()
        .stdout(predicates::str::contains("[[default.buckets]]"));
}

/// Tests that `config path` reports the configuration file location and exits successfully.
#[test]
fn test_config_path_succeeds() {
    refile_cmd()
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Configuration file path:"));
}

/// Tests that `config init` writes a config that `config validate` then accepts.
///
/// Exercises the example configuration through the real init/validate path (using an
/// isolated `XDG_CONFIG_HOME`), confirming the ordered bucket schema round-trips.
#[test]
fn test_config_init_then_validate() {
    let config_home = TempDir::new().expect("Failed to create temporary config dir");

    refile_cmd()
        .env("XDG_CONFIG_HOME", config_home.path())
        .args(["config", "init"])
        .assert()
        .success();

    refile_cmd()
        .env("XDG_CONFIG_HOME", config_home.path())
        .args(["config", "validate"])
        .assert()
        .success()
        .stdout(predicates::str::contains("current-week = current-week"));
}

/// Tests basic file organization into calendar buckets.
///
/// Pins the reference instant to Wednesday 2026-06-17 (`REFILE_NOW`) and creates one
/// file per bucket with an absolute modification time, then verifies each lands in the
/// correct bucket:
/// - 2026-06-16 → current-week/  (this week)
/// - 2026-06-10 → last-week/     (previous calendar week)
/// - 2026-06-03 → current-month/ (this month, before last week)
/// - 2026-05-15 → last-month/    (previous calendar month)
/// - 2026-04-10 → old-stuff/     (older than the previous month)
///
/// Also verifies that original files are removed from the source directory after the move.
#[test]
fn test_basic_file_organization() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    create_file_on(source, "this-week.txt", "2026-06-16T08:00:00Z")
        .expect("Failed to create this-week.txt");
    create_file_on(source, "prev-week.txt", "2026-06-10T08:00:00Z")
        .expect("Failed to create prev-week.txt");
    create_file_on(source, "this-month.txt", "2026-06-03T08:00:00Z")
        .expect("Failed to create this-month.txt");
    create_file_on(source, "prev-month.txt", "2026-05-15T08:00:00Z")
        .expect("Failed to create prev-month.txt");
    create_file_on(source, "ancient.txt", "2026-04-10T08:00:00Z")
        .expect("Failed to create ancient.txt");

    // Run refile with a pinned "now"
    refile_cmd()
        .env("REFILE_NOW", PINNED_NOW)
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Check that files were moved to correct buckets using assert_fs
    temp_dir
        .child(REFILE_BASE)
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{CURRENT_WEEK_BUCKET}/this-week.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{LAST_WEEK_BUCKET}/prev-week.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{CURRENT_MONTH_BUCKET}/this-month.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{LAST_MONTH_BUCKET}/prev-month.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/ancient.txt"))
        .assert(predicates::path::exists());

    // Original files should be gone
    for name in [
        "this-week.txt",
        "prev-week.txt",
        "this-month.txt",
        "prev-month.txt",
        "ancient.txt",
    ] {
        temp_dir.child(name).assert(predicates::path::missing());
    }
}

/// Tests dry-run mode provides safe preview capability without modifications.
///
/// **User Story**: User wants to preview what refile will do before committing changes.
///
/// **Guarantees**:
/// - No files are moved from their original location
/// - Command still exits successfully
/// - Output shows what *would* happen (not verified here, but tested in output tests)
///
/// **Critical Property**: Idempotent - running dry-run multiple times has identical effect.
#[test]
fn test_dry_run_does_not_move_files() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    create_file_with_age(source, "test.txt", RECENT_FILE_AGE).expect("Failed to create test.txt");

    // Run refile with --dry-run
    refile_cmd()
        .arg("--dry-run")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // File should still be in original location
    temp_dir
        .child("test.txt")
        .assert(predicates::path::exists());

    // In dry-run mode, no files should be moved
    // The refile directory structure may or may not be created (implementation detail),
    // but if it exists, it should not contain our test file
    let refile_dir = temp_dir.child(REFILE_BASE);
    if refile_dir.path().exists() {
        // If refile directory was created, verify test file is not in any bucket
        temp_dir
            .child(format!("{CURRENT_WEEK_BUCKET}/test.txt"))
            .assert(predicates::path::missing());
    }
}

/// Tests that file conflicts cause failure when --allow-rename is not set.
///
/// **User Story**: User wants to ensure data safety by detecting conflicts explicitly
/// rather than allowing automatic renaming.
///
/// **Scenario**: Two files with the same name need to be moved to the same bucket
/// at different times.
///
/// **Expected**: Command fails with error, preventing potential data overwrite.
#[test]
fn test_conflict_without_rename_fails() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create two files with the same name; both age 0 so both map to current-week
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE)
        .expect("Failed to create first file.txt");

    // Run refile once
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Create another file with same name
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE)
        .expect("Failed to create second file.txt");

    // Try to refile again without --allow-rename, should fail
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .failure();
}

/// Tests that --allow-rename flag handles conflicts by adding numbered suffixes.
///
/// **User Story**: User wants automatic conflict resolution without manual intervention.
///
/// **Scenario**: Two files with the same name need to be moved to the same bucket.
///
/// **Expected Behavior**:
/// - First file keeps original name: `file.txt`
/// - Second file gets numbered suffix: `file (1).txt`
/// - Both files coexist without data loss
///
/// **Verification**: Tests exact rename behavior, not just presence of some renamed file.
#[test]
fn test_allow_rename_handles_conflicts() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create first file
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE)
        .expect("Failed to create first file.txt");
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Create conflicting file (same name, same current-week bucket)
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE)
        .expect("Failed to create conflicting file.txt");

    // Run with --allow-rename
    refile_cmd()
        .arg("--allow-rename")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Both files should exist (one renamed with suffix (1))
    let current_week = source.join(CURRENT_WEEK_BUCKET);

    // Verify exactly 2 files exist in the bucket
    let entries: Vec<_> = fs::read_dir(&current_week)
        .expect("Failed to read current-week directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to iterate directory entries");
    assert_eq!(
        entries.len(),
        2,
        "Expected exactly 2 files in current-week bucket"
    );

    // Verify the original file exists
    assert!(
        current_week.join("file.txt").exists(),
        "Original file.txt should exist"
    );

    // Verify exactly one renamed file with suffix (1)
    assert!(
        current_week.join("file (1).txt").exists(),
        "Conflicting file should be renamed to file (1).txt"
    );

    // Ensure no higher numbered suffixes exist
    assert!(
        !current_week.join("file (2).txt").exists(),
        "Should not skip to suffix (2)"
    );
}

/// Tests custom base folder configuration.
///
/// **User Story**: User wants to organize files into a custom directory name
/// instead of the default "refile" folder.
///
/// **Scenario**: Use --base-folder to specify "archive" instead of "refile".
///
/// **Expected**: Files are organized into `archive/current-week/` instead of `refile/current-week/`.
#[test]
fn test_custom_base_folder() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    create_file_with_age(source, "test.txt", RECENT_FILE_AGE).expect("Failed to create test.txt");

    // Run with custom base folder
    refile_cmd()
        .arg("--base-folder")
        .arg("archive")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Check file is in custom base folder
    temp_dir
        .child("archive/current-week/test.txt")
        .assert(predicates::path::exists());
    temp_dir
        .child(REFILE_BASE)
        .assert(predicates::path::missing());
}

/// Tests custom bucket configuration via CLI.
///
/// **User Story**: User wants to define their own bucket names and calendar periods
/// instead of using the default buckets.
///
/// **Scenario**: Define custom buckets: recent (current-week), monthly (current-month),
/// archive (catch-all). Pin "now" so the mapping is deterministic.
///
/// **Expected**: Files are categorized according to the custom buckets:
/// - 2026-06-16 → recent/   (current week)
/// - 2026-06-03 → monthly/  (this month, before current week)
/// - 2026-04-10 → archive/  (catch-all)
#[test]
fn test_custom_buckets() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    create_file_on(source, "recent.txt", "2026-06-16T08:00:00Z")
        .expect("Failed to create recent.txt");
    create_file_on(source, "monthly.txt", "2026-06-03T08:00:00Z")
        .expect("Failed to create monthly.txt");
    create_file_on(source, "archive.txt", "2026-04-10T08:00:00Z")
        .expect("Failed to create archive.txt");

    // Run with custom buckets
    refile_cmd()
        .env("REFILE_NOW", PINNED_NOW)
        .arg("--buckets")
        .arg("recent=current-week,monthly=current-month,archive=null")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    temp_dir
        .child(format!("{REFILE_BASE}/recent/recent.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{REFILE_BASE}/monthly/monthly.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{REFILE_BASE}/archive/archive.txt"))
        .assert(predicates::path::exists());
}

/// Tests organizing files into a separate target directory.
///
/// **User Story**: User wants to organize files from one location into a different location
/// rather than creating subdirectories within the source.
///
/// **Scenario**: Provide both source and target directory arguments.
///
/// **Expected**:
/// - Files are moved from source to target/refile/bucket/
/// - Source directory has no refile subdirectory
/// - Original files are removed from source
#[test]
fn test_separate_target_directory() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source_dir = temp_dir.child("source");
    let target_dir = temp_dir.child("target");

    source_dir
        .create_dir_all()
        .expect("Failed to create source directory");
    target_dir
        .create_dir_all()
        .expect("Failed to create target directory");

    create_file_with_age(source_dir.path(), "test.txt", RECENT_FILE_AGE)
        .expect("Failed to create test.txt");

    // Run with separate target directory
    refile_cmd()
        .arg(
            source_dir
                .path()
                .to_str()
                .expect("Source path contains invalid UTF-8"),
        )
        .arg(
            target_dir
                .path()
                .to_str()
                .expect("Target path contains invalid UTF-8"),
        )
        .assert()
        .success();

    // File should be in target directory
    target_dir
        .child(format!("{CURRENT_WEEK_BUCKET}/test.txt"))
        .assert(predicates::path::exists());
    source_dir
        .child("test.txt")
        .assert(predicates::path::missing());
    source_dir
        .child(REFILE_BASE)
        .assert(predicates::path::missing());
}

/// Tests that directories are moved as complete units.
///
/// **User Story**: User has project directories that should be organized based on
/// the directory's age, not individual file ages.
///
/// **Scenario**: Create a directory with files inside, set the directory's mtime to be old.
///
/// **Expected**:
/// - Entire directory is moved to appropriate bucket based on directory age
/// - Files inside maintain their structure
/// - Original directory is removed from source
#[test]
fn test_directories_moved_as_whole() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create a directory with files inside
    let old_dir = source.join("old_project");
    fs::create_dir(&old_dir).expect("Failed to create old_project directory");
    create_file_with_age(&old_dir, "file1.txt", 0)
        .expect("Failed to create file1.txt with age 0 days");
    create_file_with_age(&old_dir, "file2.txt", 0)
        .expect("Failed to create file2.txt with age 0 days");

    // Make the directory itself old
    let age = SystemTime::now() - Duration::from_secs(OLD_FILE_AGE * SECONDS_PER_DAY);
    filetime::set_file_mtime(&old_dir, filetime::FileTime::from_system_time(age))
        .expect("Failed to set mtime on old_project directory");

    // Run refile
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Directory should be moved as a whole
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/old_project"))
        .assert(predicates::path::is_dir());
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/old_project/file1.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/old_project/file2.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child("old_project")
        .assert(predicates::path::missing());
}

/// Tests that empty directories are moved correctly.
///
/// **User Story**: User has empty directories (e.g., project skeletons, placeholders)
/// that should still be organized based on age.
///
/// **Scenario**: Create an empty directory and set its mtime.
///
/// **Expected**:
/// - Empty directory is moved to appropriate bucket based on its age
/// - Directory remains empty after move
/// - Original location is cleaned up
#[test]
fn test_empty_directory_handling() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create an empty directory
    let empty_dir = source.join("empty");
    fs::create_dir(&empty_dir).expect("Failed to create empty directory");

    // Make it old
    let age = SystemTime::now() - Duration::from_secs(OLD_FILE_AGE * SECONDS_PER_DAY);
    filetime::set_file_mtime(&empty_dir, filetime::FileTime::from_system_time(age))
        .expect("Failed to set mtime on empty directory");

    // Run refile
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Empty directory should be moved
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/empty"))
        .assert(predicates::path::is_dir());
    temp_dir.child("empty").assert(predicates::path::missing());
}

/// Tests idempotent behavior when refiling multiple times as files age.
///
/// **User Story**: User wants to re-organize files periodically as they age,
/// moving them from recent buckets to older buckets over time.
///
/// **Scenario**:
/// 1. Create a recent file and refile it (goes to current-week)
/// 2. Simulate time passing by changing the file's mtime to be very old
/// 3. Refile again (should move to old-stuff)
///
/// **Expected**:
/// - First run: file moves to current-week bucket
/// - After aging: file moves from current-week to old-stuff bucket
/// - No data loss, file is moved (not copied)
///
/// **Critical Property**: Demonstrates refile's ability to reorganize previously
/// organized files as they age over time.
#[test]
fn test_repeated_refiling() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create a recent file
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE).expect("Failed to create file.txt");

    // First run
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();
    assert!(source.join(CURRENT_WEEK_BUCKET).join("file.txt").exists());

    // Make the file very old (simulate time passing)
    let old_path = source.join(CURRENT_WEEK_BUCKET).join("file.txt");
    let age = SystemTime::now() - Duration::from_secs(OLD_FILE_AGE * SECONDS_PER_DAY);
    filetime::set_file_mtime(&old_path, filetime::FileTime::from_system_time(age))
        .expect("Failed to set mtime to simulate aging");

    // Second run - file should move to a different bucket
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();
    assert!(
        !source.join(CURRENT_WEEK_BUCKET).join("file.txt").exists(),
        "File still in current-week"
    );
    assert!(
        source.join(OLD_STUFF_BUCKET).join("file.txt").exists(),
        "File not moved to old-stuff"
    );
}
