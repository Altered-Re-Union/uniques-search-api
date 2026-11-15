use std::time::Duration;

use anyhow::{Context, Result};

use super::archive::TarZstIndexStorage;
use super::load_uniques_index_from;
use crate::index::UniquesIndex;

const HTTP_TIMEOUT_SECS: u64 = 120;

/// Fetches a `full_index.tar.zst` archive from a plain, unauthenticated URL
/// (e.g. a public object storage bucket). No cloud SDK, no credentials --
/// just a GET, mirroring how `formats::source::HttpFormatsSource` fetches its
/// manifest. Hot-reload isn't implemented yet (see `config::validate_settings`,
/// which rejects `index.reload.enabled = true` for this source).
#[derive(Debug, Clone)]
pub struct HttpIndexClient {
    url: String,
}

impl HttpIndexClient {
    pub fn new(url: impl Into<String>) -> Self {
        Self { url: url.into() }
    }

    pub fn url(&self) -> &str {
        &self.url
    }

    fn client() -> Result<reqwest::blocking::Client> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build()
            .context("build http client")
    }

    pub fn fetch_archive_bytes_sync(&self) -> Result<Vec<u8>> {
        let client = Self::client()?;
        let response = client
            .get(&self.url)
            .send()
            .with_context(|| format!("fetch {}", self.url))?
            .error_for_status()
            .with_context(|| format!("fetch {}", self.url))?;
        let bytes = response
            .bytes()
            .with_context(|| format!("read body {}", self.url))?;
        Ok(bytes.to_vec())
    }
}

pub fn load_uniques_index_from_http(client: &HttpIndexClient) -> Result<UniquesIndex> {
    let bytes = client.fetch_archive_bytes_sync()?;
    let storage = TarZstIndexStorage::from_bytes(&bytes, client.url())?;
    load_uniques_index_from(&storage)
}

pub fn load_index_from_http(client: &HttpIndexClient) -> Result<crate::http::state::AppState> {
    Ok(crate::http::state::AppState::new_with_index(
        load_uniques_index_from_http(client)?,
    ))
}

pub fn load_app_state_from_http(
    client: &HttpIndexClient,
    settings: &crate::config::Settings,
) -> Result<crate::http::state::AppState> {
    use super::build_app_state;

    // Non-unique/family-catalog loading isn't wired for the http source yet (same "not yet"
    // precedent as hot-reload over http, see config.rs's validate_settings).
    Ok(build_app_state(
        load_uniques_index_from_http(client)?,
        None,
        Default::default(),
        settings,
    ))
}
