use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

/// Reads a duration written as text, such as `"15m"` or `"1h 30m"`. See
/// [`parse_duration`] for the accepted forms.
fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let text = String::deserialize(deserializer)?;
    parse_duration(&text).map_err(serde::de::Error::custom)
}

/// Parses one or more `<number><unit>` terms, with optional spaces
/// between them, and adds them up: `"15m"`, `"90s"`, `"1h 30m"`.
///
/// The units are `ms`, `s`, `m`, `h`, `d` and `w`, plus the long forms
/// (`seconds`, `minutes`, `hours`, `days`, `weeks`). Fractions, negative
/// numbers, a number with no unit and an empty string are errors.
fn parse_duration(text: &str) -> Result<Duration, String> {
    let bad = |why: &str| {
        format!("invalid duration {text:?}: {why} (examples: \"15m\", \"4h\", \"1h 30m\")")
    };
    let mut rest = text.trim();
    if rest.is_empty() {
        return Err(bad("it is empty"));
    }
    let mut total = Duration::ZERO;
    while !rest.is_empty() {
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        if digits_end == 0 {
            return Err(bad("expected a number"));
        }
        let number: u64 = rest[..digits_end]
            .parse()
            .map_err(|_| bad("the number is too large"))?;
        rest = rest[digits_end..].trim_start();
        let unit_end = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let term = match &rest[..unit_end] {
            "ms" | "msec" | "millisecond" | "milliseconds" => Some(Duration::from_millis(number)),
            "s" | "sec" | "secs" | "second" | "seconds" => Some(Duration::from_secs(number)),
            "m" | "min" | "mins" | "minute" | "minutes" => {
                number.checked_mul(60).map(Duration::from_secs)
            }
            "h" | "hr" | "hrs" | "hour" | "hours" => {
                number.checked_mul(3600).map(Duration::from_secs)
            }
            "d" | "day" | "days" => number.checked_mul(86_400).map(Duration::from_secs),
            "w" | "week" | "weeks" => number.checked_mul(604_800).map(Duration::from_secs),
            "" => return Err(bad("a number needs a unit")),
            _ => return Err(bad("unknown unit")),
        };
        let term = term.ok_or_else(|| bad("the number is too large"))?;
        total = total
            .checked_add(term)
            .ok_or_else(|| bad("the total is too large"))?;
        rest = rest[unit_end..].trim_start();
    }
    Ok(total)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub media: MediaConfig,
    #[serde(default)]
    pub ssdp: SsdpConfig,
    #[serde(default)]
    pub library: LibraryConfig,
    #[serde(default)]
    pub rescan: RescanConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
    #[serde(default)]
    pub tag_cache: TagCacheConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub friendly_name: String,
    pub port: u16,
    pub interface: String,
    /// Either the literal string `"auto"` (a UUID is generated at startup) or
    /// a fixed UUID string. Validated at load time; resolved into an actual
    /// `uuid::Uuid` where it's needed (device description generation).
    pub uuid: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SsdpConfig {
    /// How often to re-send NOTIFY ssdp:alive. `CACHE-CONTROL: max-age` on
    /// every advertisement is derived from this (twice the interval), so a
    /// single missed announcement doesn't cause premature expiry on a
    /// control point.
    #[serde(deserialize_with = "deserialize_duration")]
    pub notify_interval: Duration,
}

impl SsdpConfig {
    pub fn max_age(&self) -> u32 {
        u32::try_from(self.notify_interval.as_secs().saturating_mul(2)).unwrap_or(u32::MAX)
    }
}

impl Default for SsdpConfig {
    fn default() -> Self {
        SsdpConfig {
            notify_interval: Duration::from_secs(15 * 60),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaConfig {
    pub directories: Vec<MediaDirectory>,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
}

impl MediaConfig {
    /// The configured directories' paths on their own, for callers that
    /// only need a containment boundary (`core::http`'s `verify_within_roots`,
    /// `metadata::tags::TagMetadata`'s external-cover-file lookup) and not
    /// the rest of `MediaDirectory`.
    pub fn roots(&self) -> Vec<PathBuf> {
        self.directories.iter().map(|d| d.path.clone()).collect()
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MediaDirectory {
    pub path: PathBuf,
    pub kind: MediaKind,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Audio,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LibraryConfig {
    pub views: Vec<View>,
    pub recently_added: RecentlyAddedConfig,
}

impl Default for LibraryConfig {
    fn default() -> Self {
        LibraryConfig {
            views: vec![
                View::Folders,
                View::Albums,
                View::Artists,
                View::RecentlyAddedSongs,
                View::RecentlyAddedAlbums,
            ],
            recently_added: RecentlyAddedConfig::default(),
        }
    }
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum View {
    Folders,
    AllSongs,
    Albums,
    Artists,
    RecentlyAddedSongs,
    RecentlyAddedAlbums,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RecentlyAddedConfig {
    pub songs_count: u32,
    pub albums_count: u32,
    /// 0 means no age cutoff.
    pub max_age_days: u32,
}

impl Default for RecentlyAddedConfig {
    fn default() -> Self {
        RecentlyAddedConfig {
            songs_count: 50,
            albums_count: 20,
            max_age_days: 90,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct RescanConfig {
    #[serde(deserialize_with = "deserialize_duration")]
    pub interval: Duration,
    pub on_startup: bool,
}

impl Default for RescanConfig {
    fn default() -> Self {
        RescanConfig {
            interval: Duration::from_secs(4 * 60 * 60),
            on_startup: true,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct LoggingConfig {
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        LoggingConfig {
            level: "info".to_string(),
        }
    }
}

/// A persistent on-disk cache of tag-read results (see
/// `metadata::tag_cache`), off by default. Unlike every other feature
/// added since Phase 12, this one gets an explicit toggle: enabling it
/// means writing a file to disk, and this project's own
/// `systemd/dlna-rs.service` locks the filesystem read-only by default
/// (`ProtectSystem=strict`) with no writable path granted anywhere -
/// caching can't silently assume one exists.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct TagCacheConfig {
    pub enabled: bool,
    pub path: PathBuf,
}

impl Default for TagCacheConfig {
    fn default() -> Self {
        TagCacheConfig {
            enabled: false,
            path: PathBuf::new(),
        }
    }
}

#[derive(Debug)]
pub enum ConfigError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    NoMediaDirectories,
    PrivilegedPort(u16),
    InvalidUuid(String),
    InvalidLogLevel(String),
    TagCachePathRequired,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigError::Read { path, source } => {
                write!(f, "couldn't read {}: {source}", path.display())
            }
            ConfigError::Parse { path, source } => {
                write!(f, "couldn't parse {}: {source}", path.display())
            }
            ConfigError::NoMediaDirectories => {
                write!(
                    f,
                    "media.directories is empty; configure at least one directory"
                )
            }
            ConfigError::PrivilegedPort(port) => write!(
                f,
                "server.port is {port}, but ports <= 1024 need elevated privileges; \
                 pick a port above 1024 so this never needs to run as root"
            ),
            ConfigError::InvalidUuid(value) => write!(
                f,
                "server.uuid is {value:?}, which isn't \"auto\" or a valid UUID"
            ),
            ConfigError::InvalidLogLevel(value) => write!(
                f,
                "logging.level is {value:?}; expected one of: off, error, warn, info, debug, trace"
            ),
            ConfigError::TagCachePathRequired => write!(
                f,
                "tag_cache.enabled is true, but tag_cache.path is empty; \
                 set it to a real writable path, e.g. \"/var/lib/dlna-rs/tags.redb\" \
                 (see systemd/dlna-rs.service for the matching StateDirectory= setup)"
            ),
        }
    }
}

impl std::error::Error for ConfigError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ConfigError::Read { source, .. } => Some(source),
            ConfigError::Parse { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Config = toml::from_str(&raw).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.media.directories.is_empty() {
            return Err(ConfigError::NoMediaDirectories);
        }
        if self.server.port <= 1024 {
            return Err(ConfigError::PrivilegedPort(self.server.port));
        }
        if self.server.uuid != "auto" && uuid::Uuid::parse_str(&self.server.uuid).is_err() {
            return Err(ConfigError::InvalidUuid(self.server.uuid.clone()));
        }
        if self.logging.level.parse::<log::LevelFilter>().is_err() {
            return Err(ConfigError::InvalidLogLevel(self.logging.level.clone()));
        }
        if self.tag_cache.enabled && self.tag_cache.path.as_os_str().is_empty() {
            return Err(ConfigError::TagCachePathRequired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_config(contents: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dlna-rs.toml");
        std::fs::write(&path, contents).unwrap();
        (dir, path)
    }

    const MINIMAL: &str = r#"
        [server]
        friendly_name = "test-server"
        port = 8200
        interface = "eth0"
        uuid = "auto"

        [media]
        directories = [
            { path = "/srv/media/music", kind = "audio" },
        ]
    "#;

    #[test]
    fn parses_durations_in_the_forms_the_config_uses() {
        let secs = Duration::from_secs;
        assert_eq!(parse_duration("15m"), Ok(secs(900)));
        assert_eq!(parse_duration("4h"), Ok(secs(4 * 3600)));
        assert_eq!(parse_duration("90s"), Ok(secs(90)));
        assert_eq!(parse_duration("1h 30m"), Ok(secs(5400)));
        assert_eq!(parse_duration("1h30m"), Ok(secs(5400)));
        assert_eq!(parse_duration(" 2 days "), Ok(secs(2 * 86_400)));
        assert_eq!(parse_duration("1w"), Ok(secs(604_800)));
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("0s"), Ok(Duration::ZERO));
    }

    #[test]
    fn rejects_bad_durations_with_a_message_that_names_the_value() {
        for bad in [
            "",
            "   ",
            "15",
            "m",
            "1.5h",
            "-5m",
            "5 parsecs",
            "1h x",
            "99999999999999999999s",
            "18446744073709551615w",
        ] {
            let err = parse_duration(bad).unwrap_err();
            assert!(err.contains("invalid duration"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_bad_duration_in_the_config_file_fails_loading() {
        let (_dir, path) = write_config(&format!("{MINIMAL}\n[rescan]\ninterval = \"soon\"\n"));
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("invalid duration"), "{err}");
    }

    #[test]
    fn loads_a_duration_written_as_hours_and_minutes() {
        let (_dir, path) = write_config(&format!("{MINIMAL}\n[rescan]\ninterval = \"1h 30m\"\n"));
        let config = Config::load(&path).unwrap();
        assert_eq!(config.rescan.interval, Duration::from_secs(5400));
    }

    #[test]
    fn loads_minimal_config_with_defaults() {
        let (_dir, path) = write_config(MINIMAL);
        let config = Config::load(&path).expect("minimal config should load");
        assert_eq!(config.rescan.interval, Duration::from_secs(4 * 60 * 60));
        assert_eq!(config.ssdp.notify_interval, Duration::from_secs(15 * 60));
        assert_eq!(config.ssdp.max_age(), 30 * 60);
        assert_eq!(config.logging.level, "info");
        assert_eq!(config.library.recently_added.songs_count, 50);
        assert_eq!(config.library.views.len(), 5);
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let (_dir, path) = write_config(&format!("{MINIMAL}\n[typo_section]\nfoo = 1\n"));
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }));
    }

    #[test]
    fn rejects_unknown_view_name_with_helpful_message() {
        let contents = format!("{MINIMAL}\n[library]\nviews = [\"folders\", \"recently_add\"]\n");
        let (_dir, path) = write_config(&contents);
        let err = Config::load(&path).unwrap_err().to_string();
        assert!(err.contains("recently_add"));
        assert!(
            err.contains("recently_added_songs"),
            "error should list valid options: {err}"
        );
    }

    #[test]
    fn rejects_empty_media_directories() {
        let contents = r#"
            [server]
            friendly_name = "test-server"
            port = 8200
            interface = "eth0"
            uuid = "auto"

            [media]
            directories = []
        "#;
        let (_dir, path) = write_config(contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::NoMediaDirectories
        ));
    }

    #[test]
    fn rejects_privileged_port() {
        let contents = MINIMAL.replace("port = 8200", "port = 80");
        let (_dir, path) = write_config(&contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::PrivilegedPort(80)
        ));
    }

    #[test]
    fn rejects_invalid_fixed_uuid() {
        let contents = MINIMAL.replace("uuid = \"auto\"", "uuid = \"not-a-uuid\"");
        let (_dir, path) = write_config(&contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::InvalidUuid(_)
        ));
    }

    #[test]
    fn rejects_invalid_log_level() {
        let contents = format!("{MINIMAL}\n[logging]\nlevel = \"verbose\"\n");
        let (_dir, path) = write_config(&contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::InvalidLogLevel(_)
        ));
    }

    #[test]
    fn tag_cache_defaults_to_disabled() {
        let (_dir, path) = write_config(MINIMAL);
        let config = Config::load(&path).expect("minimal config should load");
        assert!(!config.tag_cache.enabled);
    }

    #[test]
    fn tag_cache_enabled_with_a_real_path_loads() {
        let contents = format!(
            "{MINIMAL}\n[tag_cache]\nenabled = true\npath = \"/var/lib/dlna-rs/tags.redb\"\n"
        );
        let (_dir, path) = write_config(&contents);
        let config = Config::load(&path).expect("a real path should be accepted");
        assert!(config.tag_cache.enabled);
        assert_eq!(
            config.tag_cache.path,
            PathBuf::from("/var/lib/dlna-rs/tags.redb")
        );
    }

    #[test]
    fn tag_cache_enabled_with_no_path_is_rejected() {
        let contents = format!("{MINIMAL}\n[tag_cache]\nenabled = true\n");
        let (_dir, path) = write_config(&contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::TagCachePathRequired
        ));
    }

    #[test]
    fn tag_cache_rejects_unknown_field() {
        let contents = format!("{MINIMAL}\n[tag_cache]\nenabled = false\ntypo = 1\n");
        let (_dir, path) = write_config(&contents);
        assert!(matches!(
            Config::load(&path).unwrap_err(),
            ConfigError::Parse { .. }
        ));
    }
}
