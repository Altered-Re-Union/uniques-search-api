//! Tiny shared CSV-reading helper used by both the unique ([`crate::cardsdata`]) and non-unique
//! ([`crate::nonunique`]) CardsData ingestion paths.

use anyhow::{Context, Result};
use std::path::Path;

pub fn read_all<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    let mut reader =
        csv::Reader::from_path(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for record in reader.deserialize() {
        let row: T = record.with_context(|| format!("parse row in {}", path.display()))?;
        out.push(row);
    }
    Ok(out)
}
