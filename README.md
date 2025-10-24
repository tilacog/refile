# refile

Automatically organize files by age into time-based buckets.

## Usage

```bash
refile <SOURCE_DIR> [TARGET_DIR]
```

```
Organize files by age into categorized subdirectories

Usage: refile [OPTIONS] <SOURCE_DIR> [TARGET_DIR]

Arguments:
  <SOURCE_DIR>  Source directory to scan for files and directories
  [TARGET_DIR]  Target directory where refile/* subdirectories will be created (defaults to `source_dir`)

Options:
  -n, --dry-run       Perform a dry-run without moving files
  -r, --allow-rename  Allow renaming files to avoid conflicts (default: abort on conflict)
  -h, --help          Print help
  -V, --version       Print version
```

Files and directories are moved into `target/refile/` based on their age (defaults to source if target not specified):
- `last-week/` - 0-7 days old
- `current-month/` - 8-28 days old
- `last-months/` - 29-92 days old
- `old-stuff/` - 93+ days old

Directories are moved as whole units, not recursed into.

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

Running refile repeatedly will refile items again based on their current age.

## Safety

Protected directories (root, home, top-level) cannot be moved.
