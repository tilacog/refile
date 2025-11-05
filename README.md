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
      --base-folder <BASE_FOLDER>    Override base folder name (default: "refile")
      --buckets <BUCKETS>            Override bucket configuration (format: "name1=days1,name2=days2,name3=null")
  -h, --help                         Print help
  -V, --version                      Print version
```

### Default Bucket Configuration

Files and directories are moved into `target/refile/` based on their age (defaults to source if target not specified):
- `last-week/` - 0-7 days old
- `current-month/` - 8-28 days old
- `last-months/` - 29-92 days old
- `old-stuff/` - 93+ days old

**Note:** Directories are moved as whole units, not recursed into. Running `refile` repeatedly will refile items again based on their current age.

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

You can customize bucket behavior via a configuration file at `~/.config/refile/config.toml`:

```toml
# Default configuration applied to all directories
[default]
base_folder = "refile"

[default.buckets]
recent = 7
current = 30
archive = null  # null means catch-all for all older files

# Directory-specific rules
[[rules]]
path = "~/downloads"
base_folder = "sorted"

[rules.buckets]
today = 1
week = 7
old = null
```

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
refile --buckets "recent=7,month=30,old=null" ~/downloads

# Custom base folder name
refile --base-folder archive ~/documents
```

**Format:** `name1=days1,name2=days2,name3=null`
- Bucket names cannot contain `/` or `\`
- Ages must be in ascending order
- At least one bucket must have `null` (catch-all)

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
