use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct Settings {
    pub server: ServerSettings,
    pub index: IndexSettings,
    #[serde(default)]
    pub formats: Option<FormatsSettings>,
    #[serde(default)]
    pub collections: CollectionsSettings,
    #[serde(default)]
    pub cards: CardsSettings,
    #[serde(default)]
    pub artists: ArtistsSettings,
}

/// `[artists]`: where the artist credit -> searchable names mapping (`artist[]` filter,
/// `/api/v2/artists`) comes from.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ArtistsSettings {
    /// Optional CardsData `data/csv` directory (holding `Artists.csv` and
    /// `ArtistCreditMembers.csv`). When set, the mapping is built from those CSVs when the index
    /// is loaded (startup and hot-reload) and replaces the index's own `artist_credits.json`;
    /// meant for a dev environment running a prebuilt index that has no such file. Env:
    /// `ARTISTS__CARDSDATA_CSV_DIR`.
    #[serde(default)]
    pub cardsdata_csv_dir: Option<String>,
}

impl ArtistsSettings {
    pub fn cardsdata_csv_dir(&self) -> Option<PathBuf> {
        self.cardsdata_csv_dir
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CollectionsSettings {
    pub max_memory_bytes: u64,
    #[serde(default)]
    pub time_to_live_secs: u64,
    #[serde(default)]
    pub time_to_idle_secs: u64,
    /// Max POST body size for `POST /api/v2/collection/{id}`; `0` keeps Axum's default (2 MiB).
    #[serde(default)]
    pub max_post_payload_bytes: u64,
}

impl Default for CollectionsSettings {
    fn default() -> Self {
        Self {
            max_memory_bytes: 32 * 1024 * 1024,
            time_to_live_secs: 0,
            time_to_idle_secs: 0,
            max_post_payload_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CardsSettings {
    /// Max POST body size for `POST /api/v2/cards` (same params as `GET`, carried in the body);
    /// `0` keeps Axum's default (2 MiB). See `uniques-http-api/plans/25-post-cards-endpoint.md`.
    #[serde(default)]
    pub max_post_payload_bytes: u64,
}

impl Default for CardsSettings {
    fn default() -> Self {
        Self {
            max_post_payload_bytes: 0,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServerSettings {
    pub port: u16,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IndexSettings {
    pub source: IndexSourceKind,
    #[serde(default)]
    pub path: Option<String>,
    pub reload: ReloadSettings,
    #[serde(default)]
    pub object_store: ObjectStoreSettings,
    #[serde(default)]
    pub http: HttpIndexSettings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IndexSourceKind {
    Disk,
    Archive,
    ObjectStore,
    Http,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ReloadSettings {
    pub enabled: bool,
    #[serde(default)]
    pub interval_secs: Option<u64>,
}

impl ReloadSettings {
    /// Validated interval for hot-reload. Only call when `enabled` is true.
    pub fn interval_secs(&self) -> Result<u64> {
        validate_reload(self)?;
        self.interval_secs
            .context("index.reload.interval_secs missing after validation")
    }
}

fn validate_reload(reload: &ReloadSettings) -> Result<()> {
    match (reload.enabled, reload.interval_secs) {
        (true, None) => bail!(
            "index.reload.interval_secs is required when index.reload.enabled = true"
        ),
        (false, Some(_)) => bail!(
            "index.reload.interval_secs must not be set when index.reload.enabled = false"
        ),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct ObjectStoreSettings {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct HttpIndexSettings {
    /// Direct, unauthenticated URL to a `full_index.tar.zst` archive (e.g. a
    /// public object storage URL). Fetched with a plain GET, no cloud SDK.
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FormatsSettings {
    pub source: FormatsSourceConfig,
    #[serde(default)]
    pub reload_interval_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FormatsSourceConfig {
    Disk { path: String },
    Http { manifest_url: String },
}

impl FormatsSettings {
    pub fn is_enabled(&self) -> bool {
        match &self.source {
            FormatsSourceConfig::Disk { path } => !path.trim().is_empty(),
            FormatsSourceConfig::Http { manifest_url } => !manifest_url.trim().is_empty(),
        }
    }

    pub fn reload_interval_secs(&self) -> Option<u64> {
        (self.reload_interval_secs > 0).then_some(self.reload_interval_secs)
    }
}

impl Settings {
    pub fn index_path(&self) -> Result<PathBuf> {
        let path = self
            .index
            .path
            .as_deref()
            .filter(|p| !p.trim().is_empty())
            .context(
                "index.path must be set for disk/archive sources (config or INDEX_PATH env)",
            )?;
        Ok(PathBuf::from(path))
    }

    pub fn object_store_url(&self) -> Result<&str> {
        let url = self.index.object_store.url.trim();
        if url.is_empty() {
            bail!("index.object_store.url must be set when index.source = object_store");
        }
        Ok(url)
    }

    pub fn http_index_url(&self) -> Result<&str> {
        let url = self.index.http.url.trim();
        if url.is_empty() {
            bail!("index.http.url must be set when index.source = http");
        }
        Ok(url)
    }
}

pub fn load_settings() -> Result<Settings> {
    let config_dir = config_dir();
    let app_env = std::env::var("APP_ENV").unwrap_or_else(|_| "local".to_string());

    let mut settings: Settings = config::Config::builder()
        .add_source(config::File::from(config_dir.join("default.toml")))
        .add_source(
            config::File::from(config_dir.join(format!("{app_env}.toml"))).required(false),
        )
        .add_source(config::Environment::default().separator("__"))
        .build()
        .context("build config")?
        .try_deserialize()
        .context("deserialize config")?;

    apply_legacy_env_overrides(&mut settings);
    validate_settings(&settings)?;
    Ok(settings)
}

fn config_dir() -> PathBuf {
    std::env::var("CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("config"))
}

fn apply_legacy_env_overrides(settings: &mut Settings) {
    if let Ok(path) = std::env::var("INDEX_PATH") {
        if !path.trim().is_empty() {
            eprintln!("note: INDEX_PATH env override; prefer index.path in config");
            settings.index.path = Some(path);
        }
    }

    if let Ok(port) = std::env::var("PORT") {
        if let Ok(port) = port.trim().parse::<u16>() {
            settings.server.port = port;
        }
    }

    if let Ok(secs) = std::env::var("INDEX_RELOAD_INTERVAL_SECS") {
        if let Ok(secs) = secs.trim().parse::<u64>() {
            settings.index.reload.interval_secs = Some(secs);
        }
    }

    if let Ok(path) = std::env::var("FORMATS_PATH") {
        if !path.trim().is_empty() {
            eprintln!("note: FORMATS_PATH env override; prefer formats.source.path in config");
            settings.formats = Some(FormatsSettings {
                source: FormatsSourceConfig::Disk { path },
                reload_interval_secs: settings
                    .formats
                    .as_ref()
                    .map(|f| f.reload_interval_secs)
                    .unwrap_or(0),
            });
        }
    }
}

fn validate_settings(settings: &Settings) -> Result<()> {
    match settings.index.source {
        IndexSourceKind::ObjectStore => {
            settings.object_store_url()?;
        }
        IndexSourceKind::Http => {
            settings.http_index_url()?;
            if settings.index.reload.enabled {
                bail!("index.reload is not supported yet for index.source = http");
            }
        }
        IndexSourceKind::Disk | IndexSourceKind::Archive => {
            settings.index_path()?;
        }
    }
    validate_reload(&settings.index.reload)?;
    if let Some(formats) = &settings.formats {
        if formats.reload_interval_secs > 0 && !formats.is_enabled() {
            bail!("formats.reload_interval_secs requires formats.source.path to be set");
        }
    }
    validate_collections(&settings.collections)?;
    validate_cards(&settings.cards)?;
    Ok(())
}

const MAX_COLLECTION_EXPIRY_SECS: u64 = 10 * 365 * 24 * 60 * 60;

fn validate_collections(collections: &CollectionsSettings) -> Result<()> {
    if collections.max_memory_bytes == 0 {
        bail!("collections.max_memory_bytes must be greater than 0");
    }
    if collections.time_to_live_secs > MAX_COLLECTION_EXPIRY_SECS {
        bail!("collections.time_to_live_secs is too large");
    }
    if collections.time_to_idle_secs > MAX_COLLECTION_EXPIRY_SECS {
        bail!("collections.time_to_idle_secs is too large");
    }
    if collections.max_post_payload_bytes > usize::MAX as u64 {
        bail!("collections.max_post_payload_bytes exceeds platform usize::MAX");
    }
    Ok(())
}

fn validate_cards(cards: &CardsSettings) -> Result<()> {
    if cards.max_post_payload_bytes > usize::MAX as u64 {
        bail!("cards.max_post_payload_bytes exceeds platform usize::MAX");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_deserialize_from_files() {
        let settings = load_settings().expect("load settings");
        assert_eq!(settings.server.port, 8080);
        assert_eq!(settings.index.source, IndexSourceKind::Disk);
        assert!(!settings.index.reload.enabled);
        assert!(settings.index.reload.interval_secs.is_none());
        assert!(settings.index.object_store.url.is_empty());
        assert!(settings.artists.cardsdata_csv_dir().is_none());
    }

    #[test]
    fn artists_cardsdata_csv_dir_is_optional() {
        let toml = r#"
            [server]
            port = 3000

            [index]
            source = "disk"
            path = "./index"

            [index.reload]
            enabled = false

            [artists]
            cardsdata_csv_dir = " ../CardsData/data/csv "
        "#;
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(toml, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(
            settings.artists.cardsdata_csv_dir(),
            Some(PathBuf::from("../CardsData/data/csv"))
        );
        let blank = ArtistsSettings {
            cardsdata_csv_dir: Some("  ".to_string()),
        };
        assert!(blank.cardsdata_csv_dir().is_none());
    }

    #[test]
    fn disk_source_does_not_require_object_store_section() {
        let toml = r#"
            [server]
            port = 3000

            [index]
            source = "disk"
            path = "./index"

            [index.reload]
            enabled = false
        "#;
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(toml, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(settings.index.source, IndexSourceKind::Disk);
        validate_settings(&settings).expect("disk config validates without object_store");
    }

    fn http_index_toml(reload_enabled: bool) -> String {
        format!(
            r#"
            [server]
            port = 3000

            [index]
            source = "http"

            [index.http]
            url = "https://example.com/full_index.tar.zst"

            [index.reload]
            enabled = {reload_enabled}
            "#
        )
    }

    #[test]
    fn http_source_requires_url() {
        let toml = r#"
            [server]
            port = 3000

            [index]
            source = "http"

            [index.reload]
            enabled = false
        "#;
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(toml, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(settings.index.source, IndexSourceKind::Http);
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn http_source_validates_with_url_set() {
        let toml = http_index_toml(false);
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(&toml, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert_eq!(
            settings.http_index_url().unwrap(),
            "https://example.com/full_index.tar.zst"
        );
        validate_settings(&settings).expect("http config validates with url set");
    }

    #[test]
    fn http_source_rejects_reload_enabled() {
        let toml = http_index_toml(true);
        let settings: Settings = config::Config::builder()
            .add_source(config::File::from_str(&toml, config::FileFormat::Toml))
            .build()
            .unwrap()
            .try_deserialize()
            .unwrap();
        assert!(validate_settings(&settings).is_err());
    }

    #[test]
    fn reload_interval_required_when_enabled() {
        let reload = ReloadSettings {
            enabled: true,
            interval_secs: None,
        };
        assert!(validate_reload(&reload).is_err());
    }

    #[test]
    fn reload_interval_forbidden_when_disabled() {
        let reload = ReloadSettings {
            enabled: false,
            interval_secs: Some(60),
        };
        assert!(validate_reload(&reload).is_err());
    }

    #[test]
    fn reload_interval_ok_when_enabled_and_set() {
        let reload = ReloadSettings {
            enabled: true,
            interval_secs: Some(30),
        };
        assert_eq!(validate_reload(&reload).unwrap(), ());
        assert_eq!(reload.interval_secs().unwrap(), 30);
    }
}
