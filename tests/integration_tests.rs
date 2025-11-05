//! Integration tests for the refile CLI application.
//!
//! # Test Philosophy
//!
//! These integration tests verify end-to-end behavior through the CLI interface to ensure:
//!
//! 1. **Correctness**: Files are categorized and moved to correct age-based buckets
//! 2. **Safety**: Dry-run mode never modifies the filesystem
//! 3. **Conflict handling**: Rename logic prevents data loss when conflicts occur
//! 4. **Idempotency**: Repeated refiling produces correct results as files age
//! 5. **Configuration flexibility**: Custom buckets and folders work correctly
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

// Test file age constants (based on default bucket boundaries)
const RECENT_FILE_AGE: u64 = 3; // last-week bucket (0-7 days)
const MEDIUM_FILE_AGE: u64 = 15; // current-month bucket (8-28 days)
const LAST_MONTHS_AGE: u64 = 50; // last-months bucket (29-92 days)
const OLD_FILE_AGE: u64 = 100; // old-stuff bucket (93+ days)

// Bucket path constants
const REFILE_BASE: &str = "refile";
const LAST_WEEK_BUCKET: &str = "refile/last-week";
const CURRENT_MONTH_BUCKET: &str = "refile/current-month";
const LAST_MONTHS_BUCKET: &str = "refile/last-months";
const OLD_STUFF_BUCKET: &str = "refile/old-stuff";

/// Helper to create a file with a specific age (days old)
fn create_file_with_age(dir: &Path, name: &str, days_old: u64) -> std::io::Result<()> {
    let path = dir.join(name);
    std::fs::write(&path, b"test content")?;

    // Set the modification time to make the file appear older
    let age = SystemTime::now() - Duration::from_secs(days_old * SECONDS_PER_DAY);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(age))?;

    Ok(())
}

/// Helper to create a refile command
#[must_use]
fn refile_cmd() -> Command {
    Command::new(env!("CARGO_BIN_EXE_refile"))
}

/// Tests basic file organization into age-based buckets.
///
/// Creates files with different ages and verifies they are moved to the correct
/// bucket directories based on default configuration:
/// - 3-day-old file → last-week/
/// - 15-day-old file → current-month/
/// - 100-day-old file → old-stuff/
///
/// Also verifies that original files are removed from source directory after move.
#[test]
fn test_basic_file_organization() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    // Create files with different ages
    create_file_with_age(source, "recent.txt", RECENT_FILE_AGE)
        .expect("Failed to create recent.txt");
    create_file_with_age(source, "medium.txt", MEDIUM_FILE_AGE)
        .expect("Failed to create medium.txt");
    create_file_with_age(source, "old.txt", OLD_FILE_AGE).expect("Failed to create old.txt");

    // Run refile
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Check that files were moved to correct buckets using assert_fs
    temp_dir
        .child(REFILE_BASE)
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{LAST_WEEK_BUCKET}/recent.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{CURRENT_MONTH_BUCKET}/medium.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{OLD_STUFF_BUCKET}/old.txt"))
        .assert(predicates::path::exists());

    // Original files should be gone
    temp_dir
        .child("recent.txt")
        .assert(predicates::path::missing());
    temp_dir
        .child("medium.txt")
        .assert(predicates::path::missing());
    temp_dir
        .child("old.txt")
        .assert(predicates::path::missing());
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

    create_file_with_age(source, "test.txt", 5).expect("Failed to create test.txt with age 5 days");

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
            .child(format!("{LAST_WEEK_BUCKET}/test.txt"))
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

    // Create two files with the same name but different ages
    create_file_with_age(source, "file.txt", RECENT_FILE_AGE)
        .expect("Failed to create first file.txt");

    // Run refile once
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Create another file with same name
    create_file_with_age(source, "file.txt", 5)
        .expect("Failed to create second file.txt with age 5 days");

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

    // Create conflicting file
    create_file_with_age(source, "file.txt", 5).expect("Failed to create conflicting file.txt");

    // Run with --allow-rename
    refile_cmd()
        .arg("--allow-rename")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Both files should exist (one renamed with suffix (1))
    let last_week = source.join(LAST_WEEK_BUCKET);

    // Verify exactly 2 files exist in the bucket
    let entries: Vec<_> = fs::read_dir(&last_week)
        .expect("Failed to read last-week directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to iterate directory entries");
    assert_eq!(
        entries.len(),
        2,
        "Expected exactly 2 files in last-week bucket"
    );

    // Verify the original file exists
    assert!(
        last_week.join("file.txt").exists(),
        "Original file.txt should exist"
    );

    // Verify exactly one renamed file with suffix (1)
    assert!(
        last_week.join("file (1).txt").exists(),
        "Conflicting file should be renamed to file (1).txt"
    );

    // Ensure no higher numbered suffixes exist
    assert!(
        !last_week.join("file (2).txt").exists(),
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
/// **Expected**: Files are organized into `archive/last-week/` instead of `refile/last-week/`.
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
        .child("archive/last-week/test.txt")
        .assert(predicates::path::exists());
    temp_dir
        .child(REFILE_BASE)
        .assert(predicates::path::missing());
}

/// Tests custom bucket configuration.
///
/// **User Story**: User wants to define their own age boundaries and bucket names
/// instead of using the default buckets.
///
/// **Scenario**: Define custom buckets: today (≤1 day), week (≤7 days), old (everything else).
///
/// **Expected**: Files are categorized according to custom boundaries:
/// - 0-day-old file → today/
/// - 5-day-old file → week/
/// - 30-day-old file → old/
#[test]
fn test_custom_buckets() {
    let temp_dir = TempDir::new().expect("Failed to create temporary directory");
    let source = temp_dir.path();

    create_file_with_age(source, "today.txt", 0)
        .expect("Failed to create today.txt with age 0 days");
    create_file_with_age(source, "week.txt", 5).expect("Failed to create week.txt with age 5 days");
    create_file_with_age(source, "old.txt", 30).expect("Failed to create old.txt with age 30 days");

    // Run with custom buckets
    refile_cmd()
        .arg("--buckets")
        .arg("today=1,week=7,old=null")
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    temp_dir
        .child(format!("{REFILE_BASE}/today/today.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{REFILE_BASE}/week/week.txt"))
        .assert(predicates::path::exists());
    temp_dir
        .child(format!("{REFILE_BASE}/old/old.txt"))
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

    create_file_with_age(source_dir.path(), "test.txt", 5)
        .expect("Failed to create test.txt with age 5 days");

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
        .child(format!("{LAST_WEEK_BUCKET}/test.txt"))
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
    let age = SystemTime::now() - Duration::from_secs(LAST_MONTHS_AGE * SECONDS_PER_DAY);
    filetime::set_file_mtime(&empty_dir, filetime::FileTime::from_system_time(age))
        .expect("Failed to set mtime on empty directory");

    // Run refile
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();

    // Empty directory should be moved
    temp_dir
        .child(format!("{LAST_MONTHS_BUCKET}/empty"))
        .assert(predicates::path::is_dir());
    temp_dir.child("empty").assert(predicates::path::missing());
}

/// Tests idempotent behavior when refiling multiple times as files age.
///
/// **User Story**: User wants to re-organize files periodically as they age,
/// moving them from recent buckets to older buckets over time.
///
/// **Scenario**:
/// 1. Create a recent file and refile it (goes to last-week)
/// 2. Simulate time passing by changing the file's mtime
/// 3. Refile again (should move to last-months)
///
/// **Expected**:
/// - First run: file moves to last-week bucket
/// - After aging: file moves from last-week to last-months bucket
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
    assert!(source.join(LAST_WEEK_BUCKET).join("file.txt").exists());

    // Make the file older (simulate time passing)
    let old_path = source.join(LAST_WEEK_BUCKET).join("file.txt");
    let age = SystemTime::now() - Duration::from_secs(LAST_MONTHS_AGE * SECONDS_PER_DAY);
    filetime::set_file_mtime(&old_path, filetime::FileTime::from_system_time(age))
        .expect("Failed to set mtime to simulate aging");

    // Second run - file should move to different bucket
    refile_cmd()
        .arg(source.to_str().expect("Test path contains invalid UTF-8"))
        .assert()
        .success();
    assert!(
        !source.join(LAST_WEEK_BUCKET).join("file.txt").exists(),
        "File still in last-week"
    );
    assert!(
        source.join(LAST_MONTHS_BUCKET).join("file.txt").exists(),
        "File not moved to last-months"
    );
}
