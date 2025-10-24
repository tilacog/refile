# refile

Automatically organize files by age into time-based buckets.

## Usage

```
Organize files by age into categorized subdirectories

Usage: refile [OPTIONS] <SOURCE_DIR> [TARGET_DIR]

Arguments:
  <SOURCE_DIR>  Source directory to scan for files and directories
  [TARGET_DIR]  Target directory where refile/* subdirectories will be created (defaults to `source_dir`)

Options:
  -n, --dry-run                      Perform a dry-run without moving files
  -r, --allow-rename                 Allow renaming files to avoid conflicts (default: abort on conflict)
      --allow-dangerous-directories  Allow moving protected directories (root, home, top-level directories) - USE WITH EXTREME CAUTION
  -h, --help                         Print help
  -V, --version                      Print version
```

- Files and directories are moved into `target/refile/` based on their age (defaults to source if target not specified):
  - `last-week/` - 0-7 days old
  - `current-month/` - 8-28 days old
  - `last-months/` - 29-92 days old
  - `old-stuff/` - 93+ days old
- Directories are moved as whole units, not recursed into.
- Running `refile` repeatedly will refile items again based on their current age.

## Example

**Before:**
```
~/downloads/
├── report.pdf (2 days old)
├── vacation.jpg (15 days old)
└── old-backup.tar (100 days old)
```

```bash
$ refile ~/downloads
```

**After:**
```
~/downloads/
└── refile/
    ├── last-week/report.pdf
    ├── current-month/vacation.jpg
    └── old-stuff/old-backup.tar
```


## Safety

Protected directories (root `/`, home directory, and top-level directories like `/tmp`, `/var`, `/usr`) cannot be moved by default. This protection prevents accidental system damage.

**Warning**: The `--allow-dangerous-directories` flag can bypass this protection, but doing so can cause severe system damage. Only use this flag if you fully understand the consequences and have verified your source and target directories.
