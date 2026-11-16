use std::path::PathBuf;

use anyhow::Result;

use crate::index::loader::{
    load_bundle_from_storage, read_manifest, DiskIndexStorage, LoadedIndexBundle,
};

use super::source::IndexSource;

#[derive(Clone)]
pub struct DiskIndexSource {
    index_dir: PathBuf,
}

impl DiskIndexSource {
    pub fn new(index_dir: impl Into<PathBuf>) -> Self {
        Self {
            index_dir: index_dir.into(),
        }
    }
}

impl IndexSource for DiskIndexSource {
    fn read_version(&self) -> Result<u64> {
        Ok(read_manifest(&self.index_dir)?.built_at_secs)
    }

    fn load_bundle(&self) -> Result<LoadedIndexBundle> {
        load_bundle_from_storage(&DiskIndexStorage::new(&self.index_dir)?)
    }
}
