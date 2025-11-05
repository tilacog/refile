use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};
use tempfile::TempDir;

/// Helper to create a file with a specific age (days old)
fn create_file_with_age(dir: &Path, name: &str, days_old: u64) -> std::io::Result<()> {
    let path = dir.join(name);
    fs::write(&path, b"test content")?;

    // Set the modification time to make the file appear older
    let age = SystemTime::now() - Duration::from_secs(days_old * 24 * 3600);
    filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(age))?;

    Ok(())
}

/// Helper to run refile command
fn run_refile(args: &[&str]) -> std::process::Output {
    let program = env!("CARGO_BIN_EXE_refile").to_string();
    Command::new(program)
        .args(args)
        .output()
        .expect("Failed to execute refile")
}

#[test]
fn test_basic_file_organization() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create files with different ages
    create_file_with_age(source, "recent.txt", 3).unwrap();
    create_file_with_age(source, "medium.txt", 15).unwrap();
    create_file_with_age(source, "old.txt", 100).unwrap();

    // Run refile
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success(), "refile command failed");

    // Check that files were moved to correct buckets
    let refile_base = source.join("refile");
    assert!(refile_base.exists(), "refile directory not created");

    assert!(
        refile_base.join("last-week/recent.txt").exists(),
        "recent file not in last-week"
    );
    assert!(
        refile_base.join("current-month/medium.txt").exists(),
        "medium file not in current-month"
    );
    assert!(
        refile_base.join("old-stuff/old.txt").exists(),
        "old file not in old-stuff"
    );

    // Original files should be gone
    assert!(!source.join("recent.txt").exists());
    assert!(!source.join("medium.txt").exists());
    assert!(!source.join("old.txt").exists());
}

#[test]
fn test_dry_run_does_not_move_files() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    create_file_with_age(source, "test.txt", 5).unwrap();

    // Run refile with --dry-run
    let output = run_refile(&["--dry-run", source.to_str().unwrap()]);
    assert!(output.status.success(), "refile dry-run failed");

    // File should still be in original location
    assert!(
        source.join("test.txt").exists(),
        "File was moved during dry-run"
    );

    // Refile directory should not have actual files (may exist but be empty)
    let refile_base = source.join("refile");
    if refile_base.exists() {
        let last_week = refile_base.join("last-week");
        if last_week.exists() {
            assert!(
                !last_week.join("test.txt").exists(),
                "File was moved during dry-run"
            );
        }
    }
}

#[test]
fn test_conflict_without_rename_fails() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create two files with the same name but different ages
    create_file_with_age(source, "file.txt", 3).unwrap();

    // Run refile once
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());

    // Create another file with same name
    create_file_with_age(source, "file.txt", 5).unwrap();

    // Try to refile again without --allow-rename, should fail
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(!output.status.success(), "Should fail on conflict");
}

#[test]
fn test_allow_rename_handles_conflicts() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create first file
    create_file_with_age(source, "file.txt", 3).unwrap();
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());

    // Create conflicting file
    create_file_with_age(source, "file.txt", 5).unwrap();

    // Run with --allow-rename
    let output = run_refile(&["--allow-rename", source.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "Should succeed with --allow-rename"
    );

    // Both files should exist (one renamed)
    let last_week = source.join("refile/last-week");
    assert!(last_week.join("file.txt").exists());
    assert!(
        last_week.join("file (1).txt").exists()
            || last_week.join("file (2).txt").exists()
            || last_week.join("file (3).txt").exists(),
        "Renamed file not found"
    );
}

#[test]
fn test_custom_base_folder() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    create_file_with_age(source, "test.txt", 3).unwrap();

    // Run with custom base folder
    let output = run_refile(&["--base-folder", "archive", source.to_str().unwrap()]);
    assert!(output.status.success());

    // Check file is in custom base folder
    assert!(
        source.join("archive/last-week/test.txt").exists(),
        "File not in custom base folder"
    );
    assert!(
        !source.join("refile").exists(),
        "Default refile folder should not exist"
    );
}

#[test]
fn test_custom_buckets() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    create_file_with_age(source, "today.txt", 0).unwrap();
    create_file_with_age(source, "week.txt", 5).unwrap();
    create_file_with_age(source, "old.txt", 30).unwrap();

    // Run with custom buckets
    let output = run_refile(&[
        "--buckets",
        "today=1,week=7,old=null",
        source.to_str().unwrap(),
    ]);
    assert!(output.status.success());

    let refile_base = source.join("refile");
    assert!(refile_base.join("today/today.txt").exists());
    assert!(refile_base.join("week/week.txt").exists());
    assert!(refile_base.join("old/old.txt").exists());
}

#[test]
fn test_separate_target_directory() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path().join("source");
    let target = temp_dir.path().join("target");

    fs::create_dir(&source).unwrap();
    fs::create_dir(&target).unwrap();

    create_file_with_age(&source, "test.txt", 5).unwrap();

    // Run with separate target directory
    let output = run_refile(&[source.to_str().unwrap(), target.to_str().unwrap()]);
    assert!(output.status.success());

    // File should be in target directory
    assert!(
        target.join("refile/last-week/test.txt").exists(),
        "File not in target directory"
    );
    assert!(!source.join("test.txt").exists(), "File still in source");
    assert!(
        !source.join("refile").exists(),
        "Refile created in source instead of target"
    );
}

#[test]
fn test_directories_moved_as_whole() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create a directory with files inside
    let old_dir = source.join("old_project");
    fs::create_dir(&old_dir).unwrap();
    create_file_with_age(&old_dir, "file1.txt", 0).unwrap();
    create_file_with_age(&old_dir, "file2.txt", 0).unwrap();

    // Make the directory itself old
    let age = SystemTime::now() - Duration::from_secs(100 * 24 * 3600);
    filetime::set_file_mtime(&old_dir, filetime::FileTime::from_system_time(age)).unwrap();

    // Run refile
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());

    // Directory should be moved as a whole
    let moved_dir = source.join("refile/old-stuff/old_project");
    assert!(moved_dir.exists(), "Directory not moved");
    assert!(
        moved_dir.join("file1.txt").exists(),
        "File inside directory missing"
    );
    assert!(
        moved_dir.join("file2.txt").exists(),
        "File inside directory missing"
    );
    assert!(
        !source.join("old_project").exists(),
        "Original directory still exists"
    );
}

#[test]
fn test_empty_directory_handling() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create an empty directory
    let empty_dir = source.join("empty");
    fs::create_dir(&empty_dir).unwrap();

    // Make it old
    let age = SystemTime::now() - Duration::from_secs(50 * 24 * 3600);
    filetime::set_file_mtime(&empty_dir, filetime::FileTime::from_system_time(age)).unwrap();

    // Run refile
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());

    // Empty directory should be moved
    let moved_dir = source.join("refile/last-months/empty");
    assert!(moved_dir.exists(), "Empty directory not moved");
    assert!(
        !source.join("empty").exists(),
        "Original empty directory still exists"
    );
}

#[test]
fn test_repeated_refiling() {
    let temp_dir = TempDir::new().unwrap();
    let source = temp_dir.path();

    // Create a recent file
    create_file_with_age(source, "file.txt", 3).unwrap();

    // First run
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());
    assert!(source.join("refile/last-week/file.txt").exists());

    // Make the file older (simulate time passing)
    let old_path = source.join("refile/last-week/file.txt");
    let age = SystemTime::now() - Duration::from_secs(50 * 24 * 3600);
    filetime::set_file_mtime(&old_path, filetime::FileTime::from_system_time(age)).unwrap();

    // Second run - file should move to different bucket
    let output = run_refile(&[source.to_str().unwrap()]);
    assert!(output.status.success());
    assert!(
        !source.join("refile/last-week/file.txt").exists(),
        "File still in last-week"
    );
    assert!(
        source.join("refile/last-months/file.txt").exists(),
        "File not moved to last-months"
    );
}
