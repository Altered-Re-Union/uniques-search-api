use std::path::PathBuf;

use anyhow::Result;

use crate::index::loader::{
    load_bundle_from_local_path, open_index_storage, read_manifest_from, LoadedIndexBundle,
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
        let storage = open_index_storage(&self.index_dir)?;
        Ok(read_manifest_from(&storage)?.built_at_secs)
    }

    fn load_bundle(&self) -> Result<LoadedIndexBundle> {
        load_bundle_from_local_path(&self.index_dir)
    }
}
