use serde::Deserialize;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur during configuration operations.
#[derive(Debug, Error)]
pub enum ConfigError {
    /// IO error while reading or accessing configuration files.
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    /// Invalid configuration structure or values.
    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    /// Configuration file parsing error.
    #[error("Failed to parse configuration file: {0}")]
    ParseError(String),

    /// Invalid bucket specification from CLI.
    #[error("Invalid bucket specification: {0}")]
    InvalidBucketSpec(String),

    /// Invalid bucket name.
    #[error("Invalid bucket name '{0}': {1}")]
    InvalidBucketName(String, String),

    /// Missing required configuration element.
    #[error("Missing required configuration: {0}")]
    MissingConfig(String),
}

/// The calendar period a bucket captures.
///
/// Each variant maps to a *start instant* computed relative to "now" (see
/// `crate::core`). A file belongs to a bucket when its modification time is at
/// or after that start instant. Buckets are evaluated in declaration order and
/// the first match wins, so the newest periods must be listed first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boundary {
    /// From the most recent Sunday 00:00 UTC up to now.
    CurrentWeek,
    /// The previous calendar week (Sunday–Saturday) before the current week.
    LastWeek,
    /// From the first day of the current month 00:00 UTC.
    CurrentMonth,
    /// From the first day of the previous month 00:00 UTC.
    LastMonth,
    /// Catch-all: matches everything older than the other buckets.
    CatchAll,
}

/// Represents a single bucket configuration with a name and a calendar period.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketDef {
    name: String,
    boundary: Boundary,
}

impl BucketDef {
    /// Creates a new bucket definition.
    pub fn new(name: String, boundary: Boundary) -> Self {
        Self { name, boundary }
    }

    /// Returns the bucket name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the calendar period this bucket captures.
    pub fn boundary(&self) -> &Boundary {
        &self.boundary
    }

    /// Returns whether this is the catch-all bucket.
    pub fn is_catch_all(&self) -> bool {
        self.boundary == Boundary::CatchAll
    }
}

/// Runtime bucket configuration.
#[derive(Debug, Clone)]
pub struct BucketConfig {
    base_folder: String,
    buckets: Vec<BucketDef>,
}

impl BucketConfig {
    /// Returns the base folder name.
    pub fn base_folder(&self) -> &str {
        &self.base_folder
    }

    /// Returns a slice of all bucket definitions.
    pub fn buckets(&self) -> &[BucketDef] {
        &self.buckets
    }

    /// Creates a new bucket configuration (for testing).
    #[cfg(test)]
    pub fn new_for_test(base_folder: String, buckets: Vec<BucketDef>) -> Self {
        Self {
            base_folder,
            buckets,
        }
    }
}

impl Default for BucketConfig {
    /// Returns the default built-in configuration.
    fn default() -> Self {
        Self {
            base_folder: "refile".to_string(),
            buckets: vec![
                BucketDef::new("current-week".to_string(), Boundary::CurrentWeek),
                BucketDef::new("last-week".to_string(), Boundary::LastWeek),
                BucketDef::new("current-month".to_string(), Boundary::CurrentMonth),
                BucketDef::new("last-month".to_string(), Boundary::LastMonth),
                BucketDef::new("old-stuff".to_string(), Boundary::CatchAll),
            ],
        }
    }
}

impl BucketConfig {
    /// Validates the bucket configuration.
    ///
    /// Buckets are evaluated in declaration order and the first match wins, so
    /// ordering is meaningful but not statically checkable (calendar periods can
    /// overlap depending on the current date). Validation therefore only ensures
    /// the structure is sound.
    ///
    /// Returns an error if:
    /// - No buckets are defined
    /// - The catch-all (null) bucket is missing or is not the last bucket
    /// - Bucket names are empty or contain invalid characters
    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.buckets.is_empty() {
            return Err(ConfigError::InvalidConfig(
                "At least one bucket must be defined".to_string(),
            ));
        }

        // Validate bucket names
        for bucket in &self.buckets {
            if bucket.name.is_empty() {
                return Err(ConfigError::InvalidBucketName(
                    bucket.name.clone(),
                    "Bucket names cannot be empty".to_string(),
                ));
            }
            if bucket.name.contains('/') || bucket.name.contains('\\') {
                return Err(ConfigError::InvalidBucketName(
                    bucket.name.clone(),
                    "contains invalid characters (/ or \\)".to_string(),
                ));
            }
        }

        // The catch-all must exist and be the final bucket; anything declared
        // after it would be unreachable.
        match self.buckets.iter().position(BucketDef::is_catch_all) {
            None => Err(ConfigError::InvalidConfig(
                "At least one bucket must be the catch-all (null) to catch everything older"
                    .to_string(),
            )),
            Some(idx) if idx != self.buckets.len() - 1 => Err(ConfigError::InvalidConfig(
                "The catch-all (null) bucket must be the last bucket; buckets after it are unreachable"
                    .to_string(),
            )),
            Some(_) => Ok(()),
        }
    }
}

// ============================================================================
// TOML Configuration Structures
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct RefileConfigFile {
    #[serde(default)]
    default: Option<DefaultConfig>,
    #[serde(default)]
    rules: Vec<RuleConfig>,
}

#[derive(Debug, Deserialize)]
struct DefaultConfig {
    #[serde(default = "default_base_folder")]
    base_folder: String,
    buckets: Vec<BucketEntry>,
}

#[derive(Debug, Deserialize)]
struct RuleConfig {
    path: String,
    #[serde(default)]
    base_folder: Option<String>,
    buckets: Vec<BucketEntry>,
}

/// A single bucket entry as written in the config file.
///
/// Declared as an ordered array of tables (`[[default.buckets]]`) so that the
/// declaration order — which determines bucket priority — is preserved.
#[derive(Debug, Deserialize)]
struct BucketEntry {
    name: String,
    period: String,
}

fn default_base_folder() -> String {
    "refile".to_string()
}

/// Converts the ordered config-file bucket entries to `Vec<BucketDef>`,
/// parsing each `period` keyword into a [`Boundary`].
fn buckets_from_entries(entries: &[BucketEntry]) -> Result<Vec<BucketDef>, ConfigError> {
    entries
        .iter()
        .map(|entry| {
            Ok(BucketDef::new(
                entry.name.clone(),
                parse_period(&entry.period)?,
            ))
        })
        .collect()
}

/// Parses a period keyword into a [`Boundary`].
///
/// Accepted keywords: `current-week`, `last-week`, `current-month`,
/// `last-month`, and `null` (alias `catch-all`) for the catch-all bucket.
pub fn parse_period(spec: &str) -> Result<Boundary, ConfigError> {
    match spec.trim() {
        "current-week" => Ok(Boundary::CurrentWeek),
        "last-week" => Ok(Boundary::LastWeek),
        "current-month" => Ok(Boundary::CurrentMonth),
        "last-month" => Ok(Boundary::LastMonth),
        "null" | "catch-all" => Ok(Boundary::CatchAll),
        other => Err(ConfigError::InvalidBucketSpec(format!(
            "Unknown period '{other}'. Valid periods: current-week, last-week, \
             current-month, last-month, null"
        ))),
    }
}

/// Loads the refile configuration from the default config file location.
///
/// Returns Ok(None) if the config file doesn't exist.
pub fn load_config_file() -> Result<Option<RefileConfigFile>, ConfigError> {
    let config_path = config_file_path()?;

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path).map_err(|e| {
        ConfigError::Io(io::Error::new(
            e.kind(),
            format!(
                "Failed to read config file {}: {}",
                config_path.display(),
                e
            ),
        ))
    })?;

    let config: RefileConfigFile =
        toml::from_str(&contents).map_err(|e| ConfigError::ParseError(format!("{e}")))?;

    Ok(Some(config))
}

/// Returns the path to the config file: $HOME/.config/refile/config.toml
fn config_file_path() -> Result<PathBuf, ConfigError> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        ConfigError::MissingConfig("Could not determine config directory".to_string())
    })?;

    Ok(config_dir.join("refile").join("config.toml"))
}

/// Resolves the bucket configuration for a given source directory.
///
/// Precedence (highest to lowest):
/// 1. CLI overrides (`base_folder_override`, `buckets_override`)
/// 2. Directory-specific rule from config file
/// 3. Default section from config file
/// 4. Built-in default
pub fn resolve_bucket_config(
    source_dir: &Path,
    config_file: Option<&RefileConfigFile>,
    base_folder_override: Option<&str>,
    buckets_override: Option<&str>,
) -> Result<BucketConfig, ConfigError> {
    // Start with built-in default
    let mut config = BucketConfig::default();

    // Apply config file default section
    if let Some(cfg_file) = config_file {
        if let Some(default) = &cfg_file.default {
            config.base_folder.clone_from(&default.base_folder);
            config.buckets = buckets_from_entries(&default.buckets)?;
        }

        // Apply matching rule
        if let Some(rule) = find_matching_rule(source_dir, &cfg_file.rules) {
            if let Some(base) = &rule.base_folder {
                config.base_folder.clone_from(base);
            }
            config.buckets = buckets_from_entries(&rule.buckets)?;
        }
    }

    // Apply CLI overrides
    if let Some(base) = base_folder_override {
        config.base_folder = base.to_string();
    }

    if let Some(buckets_spec) = buckets_override {
        config.buckets = parse_buckets_spec(buckets_spec)?;
    }

    // Validate final configuration
    config.validate()?;

    Ok(config)
}

/// Finds a matching rule for the given source directory.
///
/// Currently does exact path matching (after canonicalization).
/// Future: could support glob patterns.
fn find_matching_rule<'a>(source_dir: &Path, rules: &'a [RuleConfig]) -> Option<&'a RuleConfig> {
    let canonical_source = fs::canonicalize(source_dir).ok()?;

    for rule in rules {
        // Expand tilde in rule path
        let rule_path = expand_tilde(&rule.path);
        if let Ok(canonical_rule) = fs::canonicalize(&rule_path)
            && canonical_source == canonical_rule
        {
            return Some(rule);
        }
    }

    None
}

/// Expands ~ to the user's home directory.
fn expand_tilde(path: &str) -> PathBuf {
    if path.starts_with("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(&path[2..]);
    }
    PathBuf::from(path)
}

/// Parses a bucket specification string from CLI.
///
/// Format: "name1=period1,name2=period2,name3=null"
/// Example: "current-week=current-week,recent=last-week,old=null"
pub fn parse_buckets_spec(spec: &str) -> Result<Vec<BucketDef>, ConfigError> {
    let mut buckets = Vec::new();

    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        let mut split = part.splitn(2, '=');
        let name = split
            .next()
            .ok_or_else(|| {
                ConfigError::InvalidBucketSpec(format!("Invalid bucket spec: '{part}'"))
            })?
            .trim();

        let period_str = split
            .next()
            .ok_or_else(|| {
                ConfigError::InvalidBucketSpec(format!(
                    "Invalid bucket spec, missing '=' in: '{part}'"
                ))
            })?
            .trim();

        let boundary = parse_period(period_str)?;

        buckets.push(BucketDef::new(name.to_string(), boundary));
    }

    if buckets.is_empty() {
        return Err(ConfigError::InvalidBucketSpec(
            "Bucket spec cannot be empty".to_string(),
        ));
    }

    Ok(buckets)
}

/// Returns the path to the config file: $HOME/.config/refile/config.toml
///
/// This is a public function that can be used by CLI commands.
pub fn get_config_file_path() -> Result<PathBuf, ConfigError> {
    config_file_path()
}

/// Returns the embedded example configuration file content.
pub fn get_example_config() -> &'static str {
    include_str!("../example-config.toml")
}

/// Writes the example configuration to the specified path.
///
/// # Arguments
///
/// * `path` - The path where the config file should be written
/// * `force` - If true, overwrite existing file; if false, fail if file exists
///
/// # Errors
///
/// Returns an error if:
/// - The file already exists and force is false
/// - The parent directory cannot be created
/// - The file cannot be written
pub fn write_default_config(path: &Path, force: bool) -> Result<(), ConfigError> {
    // Check if file exists
    if path.exists() && !force {
        return Err(ConfigError::MissingConfig(format!(
            "Config file already exists at {}. Use --force to overwrite.",
            path.display()
        )));
    }

    // Create parent directory if it doesn't exist
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            ConfigError::MissingConfig(format!(
                "Failed to create config directory {}: {}",
                parent.display(),
                e
            ))
        })?;
    }

    // Write the example config
    std::fs::write(path, get_example_config()).map_err(|e| {
        ConfigError::MissingConfig(format!(
            "Failed to write config file to {}: {}",
            path.display(),
            e
        ))
    })?;

    Ok(())
}

/// Validates the configuration file and returns a detailed result.
///
/// This function loads and validates the config file, providing detailed
/// information about any errors or the configuration structure.
///
/// # Returns
///
/// Returns `Ok(String)` with a summary of the configuration if valid,
/// or `Err(ConfigError)` with details about what's wrong.
pub fn validate_config_file() -> Result<String, ConfigError> {
    let config_path = config_file_path()?;

    // Check if config file exists
    if !config_path.exists() {
        return Err(ConfigError::MissingConfig(format!(
            "Config file does not exist at: {}",
            config_path.display()
        )));
    }

    // Try to load the config
    let config = load_config_file()?;

    match config {
        Some(config) => {
            let mut summary = String::new();
            writeln!(
                summary,
                "✓ Config file is valid: {}\n",
                config_path.display()
            )
            .expect("Writing to String should not fail");

            // Summarize default section
            if let Some(default) = &config.default {
                summary.push_str("Default configuration:\n");
                writeln!(summary, "  Base folder: {}", default.base_folder)
                    .expect("Writing to String should not fail");
                summary.push_str("  Buckets:\n");

                for entry in &default.buckets {
                    writeln!(summary, "    - {} = {}", entry.name, entry.period)
                        .expect("Writing to String should not fail");
                }
                summary.push('\n');
            }

            // Summarize rules
            if !config.rules.is_empty() {
                writeln!(summary, "Directory-specific rules: {}", config.rules.len())
                    .expect("Writing to String should not fail");
                for (i, rule) in config.rules.iter().enumerate() {
                    writeln!(summary, "  Rule {}:", i + 1)
                        .expect("Writing to String should not fail");
                    writeln!(summary, "    Path: {}", rule.path)
                        .expect("Writing to String should not fail");
                    let base_folder = rule.base_folder.as_deref().unwrap_or("refile");
                    writeln!(summary, "    Base folder: {base_folder}")
                        .expect("Writing to String should not fail");
                    summary.push_str("    Buckets:\n");
                    for entry in &rule.buckets {
                        writeln!(summary, "      - {} = {}", entry.name, entry.period)
                            .expect("Writing to String should not fail");
                    }
                }
            }

            Ok(summary)
        }
        None => Err(ConfigError::MissingConfig(
            "Config file exists but is empty or invalid".to_string(),
        )),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = BucketConfig::default();
        assert_eq!(config.base_folder(), "refile");
        assert_eq!(config.buckets().len(), 5);
        assert_eq!(config.buckets()[0].boundary(), &Boundary::CurrentWeek);
        assert_eq!(config.buckets()[4].boundary(), &Boundary::CatchAll);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_validate_no_buckets() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_no_catchall() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![
                BucketDef::new("bucket1".to_string(), Boundary::CurrentWeek),
                BucketDef::new("bucket2".to_string(), Boundary::LastWeek),
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_catchall_must_be_last() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![
                BucketDef::new("everything".to_string(), Boundary::CatchAll),
                BucketDef::new("unreachable".to_string(), Boundary::CurrentWeek),
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_bucket_name() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![
                BucketDef::new("bucket/invalid".to_string(), Boundary::CurrentWeek),
                BucketDef::new("old".to_string(), Boundary::CatchAll),
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_period() {
        assert_eq!(parse_period("current-week").unwrap(), Boundary::CurrentWeek);
        assert_eq!(parse_period("last-week").unwrap(), Boundary::LastWeek);
        assert_eq!(
            parse_period(" current-month ").unwrap(),
            Boundary::CurrentMonth
        );
        assert_eq!(parse_period("last-month").unwrap(), Boundary::LastMonth);
        assert_eq!(parse_period("null").unwrap(), Boundary::CatchAll);
        assert_eq!(parse_period("catch-all").unwrap(), Boundary::CatchAll);
        assert!(parse_period("7").is_err());
        assert!(parse_period("yesterday").is_err());
    }

    #[test]
    fn test_parse_buckets_spec() {
        let spec = "recent=current-week,prev=last-week,old=null";
        let buckets = parse_buckets_spec(spec).unwrap();

        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[0].name(), "recent");
        assert_eq!(buckets[0].boundary(), &Boundary::CurrentWeek);
        assert_eq!(buckets[1].name(), "prev");
        assert_eq!(buckets[1].boundary(), &Boundary::LastWeek);
        assert_eq!(buckets[2].name(), "old");
        assert_eq!(buckets[2].boundary(), &Boundary::CatchAll);
    }

    #[test]
    fn test_parse_buckets_spec_with_spaces() {
        let spec = " recent = current-week , prev = last-week , old = null ";
        let buckets = parse_buckets_spec(spec).unwrap();
        assert_eq!(buckets.len(), 3);
    }

    #[test]
    fn test_parse_buckets_spec_invalid() {
        assert!(parse_buckets_spec("invalid").is_err());
        assert!(parse_buckets_spec("name=7").is_err());
        assert!(parse_buckets_spec("").is_err());
    }

    #[test]
    fn test_expand_tilde() {
        let path = expand_tilde("~/test/path");
        assert!(path.to_string_lossy().contains("test/path"));
        assert!(!path.to_string_lossy().contains('~'));

        let path = expand_tilde("/absolute/path");
        assert_eq!(path, PathBuf::from("/absolute/path"));
    }
}
