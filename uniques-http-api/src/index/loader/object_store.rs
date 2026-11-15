use std::future::Future;
use std::sync::{Arc, Mutex};

use anyhow::{bail, Context, Result};
use object_store::path::Path as ObjectPath;
use object_store::{GetOptions, ObjectStore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::archive::TarZstIndexStorage;

pub const VERSION_OBJECT: &str = "version.json";
pub const ARCHIVE_OBJECT: &str = "full_index.tar.zst";

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IndexVersionSidecar {
    pub version: u64,
    pub archive_object: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VersionFetch {
    pub sidecar: IndexVersionSidecar,
    pub not_modified: bool,
}

#[derive(Clone)]
pub struct ObjectStoreIndexClient {
    store: Arc<dyn ObjectStore>,
    prefix: ObjectPath,
    source_url: String,
    cached_etag: Arc<Mutex<Option<String>>>,
    cached_version: Arc<Mutex<Option<u64>>>,
}

impl ObjectStoreIndexClient {
    pub fn new(url: &str) -> Result<Self> {
        let parsed = url::Url::parse(url).with_context(|| format!("parse url {url}"))?;
        let (store, prefix) = object_store::parse_url(&parsed)
            .with_context(|| format!("parse object store url {url}"))?;
        Ok(Self::from_parts(
            Arc::from(store),
            prefix,
            url.to_string(),
        ))
    }

    pub fn from_parts(
        store: Arc<dyn ObjectStore>,
        prefix: ObjectPath,
        source_url: impl Into<String>,
    ) -> Self {
        Self {
            store,
            prefix,
            source_url: source_url.into(),
            cached_etag: Arc::new(Mutex::new(None)),
            cached_version: Arc::new(Mutex::new(None)),
        }
    }

    pub fn archive_source_label(&self) -> String {
        format!("{}/{}", self.source_url.trim_end_matches('/'), ARCHIVE_OBJECT)
    }

    pub fn fetch_version_sync(&self) -> Result<VersionFetch> {
        block_on(self.fetch_version())
    }

    pub fn fetch_archive_bytes_sync(&self, sidecar: &IndexVersionSidecar) -> Result<Vec<u8>> {
        block_on(self.fetch_archive_bytes(sidecar))
    }

    pub fn read_version_sync(&self) -> Result<u64> {
        let fetch = self.fetch_version_sync()?;
        if fetch.not_modified {
            self.cached_version
                .lock()
                .expect("version cache lock")
                .context("version sidecar not modified but no cached version")
        } else {
            Ok(fetch.sidecar.version)
        }
    }

    async fn fetch_version(&self) -> Result<VersionFetch> {
        let path = self.prefix.child(VERSION_OBJECT);
        let if_none_match = self.cached_etag.lock().expect("etag lock").clone();
        let options = GetOptions {
            if_none_match,
            ..Default::default()
        };

        match self.store.get_opts(&path, options).await {
            Ok(result) => {
                let etag = result.meta.e_tag.clone();
                let bytes = result.bytes().await.context("read version.json body")?;
                let sidecar: IndexVersionSidecar =
                    serde_json::from_slice(&bytes).context("parse version.json")?;

                if let Ok(mut cached_etag) = self.cached_etag.lock() {
                    *cached_etag = etag;
                }
                if let Ok(mut cached_version) = self.cached_version.lock() {
                    *cached_version = Some(sidecar.version);
                }

                Ok(VersionFetch {
                    sidecar,
                    not_modified: false,
                })
            }
            Err(object_store::Error::NotModified { .. }) => Ok(VersionFetch {
                sidecar: IndexVersionSidecar {
                    version: self
                        .cached_version
                        .lock()
                        .expect("version cache lock")
                        .context("version sidecar not modified but no cached version")?,
                    archive_object: self.expected_archive_object(),
                    sha256: None,
                },
                not_modified: true,
            }),
            Err(error) => Err(error.into()),
        }
    }

    async fn fetch_archive_bytes(&self, sidecar: &IndexVersionSidecar) -> Result<Vec<u8>> {
        let path = self.archive_object_path(sidecar)?;
        let result = self
            .store
            .get(&path)
            .await
            .with_context(|| format!("get archive object {}", path))?;
        let bytes = result
            .bytes()
            .await
            .context("read archive object body")?
            .to_vec();

        if let Some(expected) = sidecar.sha256.as_deref() {
            verify_sha256(&bytes, expected)?;
        }

        Ok(bytes)
    }

    fn archive_object_path(&self, sidecar: &IndexVersionSidecar) -> Result<ObjectPath> {
        let expected = self.expected_archive_object();
        if sidecar.archive_object != expected {
            eprintln!(
                "note: version.json archive_object={} differs from expected {}; using expected path",
                sidecar.archive_object, expected
            );
        }
        Ok(self.prefix.child(ARCHIVE_OBJECT))
    }

    fn expected_archive_object(&self) -> String {
        if self.prefix.as_ref().is_empty() {
            ARCHIVE_OBJECT.to_string()
        } else {
            format!("{}/{}", self.prefix, ARCHIVE_OBJECT)
        }
    }
}

pub fn load_index_from_object_store(client: &ObjectStoreIndexClient) -> Result<crate::http::state::AppState> {
    Ok(crate::http::state::AppState::new_with_index(load_uniques_index_from_object_store(client)?))
}

pub fn load_uniques_index_from_object_store(client: &ObjectStoreIndexClient) -> Result<crate::index::UniquesIndex> {
    use super::load_uniques_index_from;

    let fetch = client.fetch_version_sync()?;
    let bytes = client.fetch_archive_bytes_sync(&fetch.sidecar)?;
    let storage = TarZstIndexStorage::from_bytes(&bytes, client.archive_source_label())?;
    load_uniques_index_from(&storage)
}

pub fn load_app_state_from_object_store(
    client: &ObjectStoreIndexClient,
    settings: &crate::config::Settings,
) -> Result<crate::http::state::AppState> {
    use super::build_app_state;

    // Non-unique/family-catalog loading isn't wired for the object_store source yet, same as http.
    Ok(build_app_state(
        load_uniques_index_from_object_store(client)?,
        None,
        Default::default(),
        settings,
    ))
}

fn verify_sha256(bytes: &[u8], expected: &str) -> Result<()> {
    let digest = Sha256::digest(bytes);
    let actual = hex_encode(&digest);
    if actual != expected.to_ascii_lowercase() {
        bail!("archive sha256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn block_on<F: Future>(future: F) -> F::Output {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => tokio::task::block_in_place(|| handle.block_on(future)),
        Err(_) => tokio::runtime::Runtime::new()
            .expect("tokio runtime")
            .block_on(future),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bytes::Bytes;
    use object_store::memory::InMemory;
    use object_store::path::Path as ObjectPath;
    use object_store::{ObjectStore, PutPayload};

    use super::*;

    #[tokio::test]
    async fn fetch_version_reads_sidecar_and_caches_etag() {
        let store = Arc::new(InMemory::new());
        let sidecar = IndexVersionSidecar {
            version: 42,
            archive_object: "index/full_index.tar.zst".to_string(),
            sha256: None,
        };
        store
            .put(
                &ObjectPath::from("index/version.json"),
                PutPayload::from_bytes(Bytes::from(serde_json::to_vec(&sidecar).unwrap())),
            )
            .await
            .unwrap();

        let client = ObjectStoreIndexClient::from_parts(
            store,
            ObjectPath::from("index"),
            "memory://index",
        );

        let fetch = client.fetch_version().await.unwrap();
        assert_eq!(fetch.sidecar.version, 42);
        assert!(!fetch.not_modified);
        assert!(client.cached_etag.lock().unwrap().is_some());
    }
}
