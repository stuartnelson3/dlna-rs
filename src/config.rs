use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: ServerConfig,
    pub media: MediaConfig,
    #[serde(default)]
    pub ssdp: SsdpConfig,
    // Read by the music library views (Phase 8).
    #[serde(default)]
    #[allow(dead_code)]
    pub library: LibraryConfig,
    // Read by the rescan timer loop (Phase 7).
    #[serde(default)]
    #[allow(dead_code)]
    pub rescan: RescanConfig,
    #[serde(default)]
    pub logging: LoggingConfig,
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
    #[serde(with = "humantime_serde")]
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

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// follow_symlinks/exclude_patterns are read by the scanner in Phase 4.
#[allow(dead_code)]
pub struct MediaConfig {
    pub directories: Vec<MediaDirectory>,
    #[serde(default)]
    pub follow_symlinks: bool,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
// Read by the scanner/FolderMirror ContentSource starting in Phase 4.
#[allow(dead_code)]
pub struct MediaDirectory {
    pub path: PathBuf,
    pub kind: MediaKind,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
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
    #[serde(with = "humantime_serde")]
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
}
