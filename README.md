# refile

Automatically organize files into calendar-based buckets by modification time
(current week, last week, current month, …). Everything is computed in UTC and
weeks start on Sunday.

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
      --base-folder <BASE_FOLDER>    Override base folder name (default: "refile")
      --buckets <BUCKETS>            Override bucket configuration (format: "name1=period1,name2=period2,name3=null")
  -h, --help                         Print help
  -V, --version                      Print version
```

### Default Bucket Configuration

Files and directories are moved into `target/refile/` based on the calendar
period their modification time falls into (defaults to source if target not
specified). Buckets are evaluated top to bottom and the first match wins:

- `current-week/` - modified since the most recent Sunday 00:00 UTC
- `last-week/` - modified in the previous Sunday–Saturday week
- `current-month/` - modified earlier this calendar month (before last week)
- `last-month/` - modified in the previous calendar month
- `old-stuff/` - everything older (catch-all)

Because the periods are anchored to the calendar, the bucket an item lands in
changes as time passes. Periods can also overlap near month boundaries (for
example, when the current week started in the previous month); the
earlier-listed bucket wins, which is why bucket order matters.

**Note:** Directories are moved as whole units, not recursed into. Running `refile` repeatedly will refile items again based on their current modification time.

## Configuration

### Configuration Management

Refile provides convenient commands to manage your configuration file:

```bash
# Create a default configuration file
refile config init

# Overwrite existing configuration (use with caution)
refile config init --force

# Print example configuration to stdout (useful for piping)
refile config dump

# Show where the config file is located
refile config path

# Validate your configuration file
refile config validate
```

### Configuration File

You can customize bucket behavior via a configuration file at `~/.config/refile/config.toml`.

Buckets are an **ordered** list of `[[default.buckets]]` tables, each with a
`name` (the folder created) and a `period` (what it captures). The order is
significant: the first matching bucket wins, and the last bucket must be the
catch-all (`period = "null"`).

```toml
# Default configuration applied to all directories
[default]
base_folder = "refile"

[[default.buckets]]
name = "current-week"
period = "current-week"

[[default.buckets]]
name = "current-month"
period = "current-month"

[[default.buckets]]
name = "archive"
period = "null"   # null means catch-all for all older files (must be last)

# Directory-specific rules
[[rules]]
path = "~/downloads"
base_folder = "sorted"

[[rules.buckets]]
name = "this-week"
period = "current-week"

[[rules.buckets]]
name = "old"
period = "null"
```

**Valid periods:** `current-week`, `last-week`, `current-month`, `last-month`,
and `null` (catch-all).

### Configuration Precedence

Settings are applied in the following order (highest to lowest priority):
1. CLI arguments (`--base-folder`, `--buckets`)
2. Directory-specific rules in config file
3. Default section in config file
4. Built-in defaults

### Custom Buckets via CLI

Override bucket configuration on the command line:

```bash
# Simple 3-bucket setup
refile --buckets "recent=current-week,month=current-month,old=null" ~/downloads

# Custom base folder name
refile --base-folder archive ~/documents
```

**Format:** `name1=period1,name2=period2,name3=null`
- Bucket names cannot contain `/` or `\`
- Newest periods first; the first matching bucket wins
- The last bucket must be the catch-all (`null`)

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

**After** (assuming today is mid-month):
```
~/downloads/
└── refile/
    ├── current-week/report.pdf
    ├── current-month/vacation.jpg
    └── old-stuff/old-backup.tar
```


## Safety

Protected directories (root `/`, home directory, and top-level directories like `/tmp`, `/var`, `/usr`) cannot be moved by default. This protection prevents accidental system damage.

**Warning**: The `--allow-dangerous-directories` flag can bypass this protection, but doing so can cause severe system damage. Only use this flag if you fully understand the consequences and have verified your source and target directories.
