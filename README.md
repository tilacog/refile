# refile

Automatically organize files by age into time-based buckets.

## Usage

```bash
refile --source /path/to/scan --target /path/to/organize
```

Files are moved into `target/refile/` based on their age:
- `last-week/` - 0-7 days old
- `current-month/` - 8-28 days old
- `last-months/` - 29-92 days old
- `old-stuff/` - 93+ days old

## Options

- `--dry-run` - Preview actions without moving files
- `--allow-rename` - Rename files on conflict (default: abort)

## Safety

Protected directories (root, home, top-level) cannot be moved.
