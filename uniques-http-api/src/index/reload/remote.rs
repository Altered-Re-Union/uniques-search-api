use anyhow::Result;

use crate::index::loader::{
    load_bundle_from_object_store, LoadedIndexBundle, ObjectStoreIndexClient,
};

use super::source::IndexSource;

#[derive(Clone)]
pub struct RemoteIndexSource {
    client: ObjectStoreIndexClient,
}

impl RemoteIndexSource {
    pub fn new(client: ObjectStoreIndexClient) -> Self {
        Self { client }
    }
}

impl IndexSource for RemoteIndexSource {
    fn read_version(&self) -> Result<u64> {
        self.client.read_version_sync()
    }

    fn load_bundle(&self) -> Result<LoadedIndexBundle> {
        load_bundle_from_object_store(&self.client)
    }
}
