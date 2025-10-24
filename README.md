# refile

Automatically organize files by age into time-based buckets.

## Usage

```bash
refile --source /path/to/scan --target /path/to/organize
```

Files and directories are moved into `target/refile/` based on their age:
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

**After:**
```
~/downloads/
└── refile/
    ├── last-week/report.pdf
    ├── current-month/vacation.jpg
    └── old-stuff/old-backup.tar
```

## Options

- `--dry-run` - Preview actions without moving files
- `--allow-rename` - Rename files on conflict (default: abort)

## Safety

Protected directories (root, home, top-level) cannot be moved.
