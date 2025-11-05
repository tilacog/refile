use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Represents a single bucket configuration with name and maximum age.
#[derive(Debug, Clone, PartialEq)]
pub struct BucketDef {
    pub name: String,
    pub max_age_days: Option<u64>, // None means infinity (catch-all)
}

/// Runtime bucket configuration.
#[derive(Debug, Clone)]
pub struct BucketConfig {
    pub base_folder: String,
    pub buckets: Vec<BucketDef>,
}

impl BucketConfig {
    /// Returns the default built-in configuration.
    pub fn default() -> Self {
        Self {
            base_folder: "refile".to_string(),
            buckets: vec![
                BucketDef {
                    name: "last-week".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "current-month".to_string(),
                    max_age_days: Some(28),
                },
                BucketDef {
                    name: "last-months".to_string(),
                    max_age_days: Some(92),
                },
                BucketDef {
                    name: "old-stuff".to_string(),
                    max_age_days: None,
                },
            ],
        }
    }

    /// Validates the bucket configuration.
    ///
    /// Returns an error if:
    /// - No buckets are defined
    /// - Age thresholds are not in ascending order
    /// - No catch-all bucket (with None age) exists
    /// - Bucket names contain invalid characters
    pub fn validate(&self) -> io::Result<()> {
        if self.buckets.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "At least one bucket must be defined",
            ));
        }

        // Check for catch-all bucket
        if !self.buckets.iter().any(|b| b.max_age_days.is_none()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "At least one bucket must have no age limit (null) to catch all old files",
            ));
        }

        // Validate bucket names
        for bucket in &self.buckets {
            if bucket.name.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Bucket names cannot be empty",
                ));
            }
            if bucket.name.contains('/') || bucket.name.contains('\\') {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Bucket name '{}' contains invalid characters", bucket.name),
                ));
            }
        }

        // Check that ages are in ascending order (excluding None)
        let mut prev_age: Option<u64> = None;
        for bucket in &self.buckets {
            if let Some(age) = bucket.max_age_days {
                if let Some(prev) = prev_age
                    && age <= prev
                {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("Bucket ages must be in ascending order: {age} <= {prev}"),
                    ));
                }
                prev_age = Some(age);
            }
        }

        Ok(())
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
    buckets: BTreeMap<String, Option<u64>>,
}

#[derive(Debug, Deserialize)]
struct RuleConfig {
    path: String,
    #[serde(default)]
    base_folder: Option<String>,
    buckets: BTreeMap<String, Option<u64>>,
}

fn default_base_folder() -> String {
    "refile".to_string()
}

/// Converts a `BTreeMap` of bucket definitions to a Vec<BucketDef>.
fn buckets_from_map(map: BTreeMap<String, Option<u64>>) -> Vec<BucketDef> {
    map.into_iter()
        .map(|(name, max_age_days)| BucketDef { name, max_age_days })
        .collect()
}

/// Loads the refile configuration from the default config file location.
///
/// Returns Ok(None) if the config file doesn't exist.
pub fn load_config_file() -> io::Result<Option<RefileConfigFile>> {
    let config_path = config_file_path()?;

    if !config_path.exists() {
        return Ok(None);
    }

    let contents = fs::read_to_string(&config_path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "Failed to read config file {}: {}",
                config_path.display(),
                e
            ),
        )
    })?;

    let config: RefileConfigFile = toml::from_str(&contents).map_err(|e| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Failed to parse config file: {e}"),
        )
    })?;

    Ok(Some(config))
}

/// Returns the path to the config file: $HOME/.config/refile/config.toml
fn config_file_path() -> io::Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Could not determine config directory",
        )
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
) -> io::Result<BucketConfig> {
    // Start with built-in default
    let mut config = BucketConfig::default();

    // Apply config file default section
    if let Some(cfg_file) = config_file {
        if let Some(default) = &cfg_file.default {
            config.base_folder.clone_from(&default.base_folder);
            config.buckets = buckets_from_map(default.buckets.clone());
        }

        // Apply matching rule
        if let Some(rule) = find_matching_rule(source_dir, &cfg_file.rules) {
            if let Some(base) = &rule.base_folder {
                config.base_folder.clone_from(base);
            }
            config.buckets = buckets_from_map(rule.buckets.clone());
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
/// Format: "name1=days1,name2=days2,name3=null"
/// Example: "today=1,week=7,old=null"
pub fn parse_buckets_spec(spec: &str) -> io::Result<Vec<BucketDef>> {
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
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Invalid bucket spec: '{part}'"),
                )
            })?
            .trim();

        let age_str = split
            .next()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Invalid bucket spec, missing '=' in: '{part}'"),
                )
            })?
            .trim();

        let max_age_days = if age_str == "null" {
            None
        } else {
            Some(age_str.parse::<u64>().map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("Invalid age value '{age_str}': {e}"),
                )
            })?)
        };

        buckets.push(BucketDef {
            name: name.to_string(),
            max_age_days,
        });
    }

    if buckets.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Bucket spec cannot be empty",
        ));
    }

    Ok(buckets)
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
        assert_eq!(config.base_folder, "refile");
        assert_eq!(config.buckets.len(), 4);
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
                BucketDef {
                    name: "bucket1".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "bucket2".to_string(),
                    max_age_days: Some(14),
                },
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_ages_not_ascending() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![
                BucketDef {
                    name: "bucket1".to_string(),
                    max_age_days: Some(14),
                },
                BucketDef {
                    name: "bucket2".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "bucket3".to_string(),
                    max_age_days: None,
                },
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_validate_invalid_bucket_name() {
        let config = BucketConfig {
            base_folder: "test".to_string(),
            buckets: vec![
                BucketDef {
                    name: "bucket/invalid".to_string(),
                    max_age_days: Some(7),
                },
                BucketDef {
                    name: "old".to_string(),
                    max_age_days: None,
                },
            ],
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_parse_buckets_spec() {
        let spec = "today=1,week=7,old=null";
        let buckets = parse_buckets_spec(spec).unwrap();

        assert_eq!(buckets.len(), 3);
        assert_eq!(buckets[0].name, "today");
        assert_eq!(buckets[0].max_age_days, Some(1));
        assert_eq!(buckets[1].name, "week");
        assert_eq!(buckets[1].max_age_days, Some(7));
        assert_eq!(buckets[2].name, "old");
        assert_eq!(buckets[2].max_age_days, None);
    }

    #[test]
    fn test_parse_buckets_spec_with_spaces() {
        let spec = " today = 1 , week = 7 , old = null ";
        let buckets = parse_buckets_spec(spec).unwrap();
        assert_eq!(buckets.len(), 3);
    }

    #[test]
    fn test_parse_buckets_spec_invalid() {
        assert!(parse_buckets_spec("invalid").is_err());
        assert!(parse_buckets_spec("name=abc").is_err());
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
