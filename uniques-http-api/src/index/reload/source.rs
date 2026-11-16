use anyhow::Result;

use crate::index::loader::LoadedIndexBundle;

pub trait IndexSource: Send + Sync + Clone {
    fn read_version(&self) -> Result<u64>;
    fn load_bundle(&self) -> Result<LoadedIndexBundle>;
}
