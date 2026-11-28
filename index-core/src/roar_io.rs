use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use std::fs;
use std::path::Path;

/// Deserialize a RoaringBitmap from a `.roar` file on disk.
pub fn read_roar(path: &Path) -> Result<RoaringBitmap> {
    let bytes = fs::read(path).with_context(|| format!("read {}", path.display()))?;
    RoaringBitmap::deserialize_from(&bytes[..])
        .with_context(|| format!("deserialize {}", path.display()))
}

/// Serialize a RoaringBitmap to a `.roar` file on disk, returning the byte count written.
pub fn write_roar(path: &Path, bitmap: &RoaringBitmap) -> Result<u64> {
    let mut bytes = Vec::new();
    bitmap
        .serialize_into(&mut bytes)
        .with_context(|| format!("serialize {}", path.display()))?;
    let len = bytes.len() as u64;
    fs::write(path, bytes).with_context(|| format!("write {}", path.display()))?;
    Ok(len)
}

/// Write a RoaringBitmap only if non-empty; returns `Ok(None)` without creating a file when empty.
pub fn write_roar_if_nonempty(path: &Path, bitmap: &RoaringBitmap) -> Result<Option<u64>> {
    if bitmap.is_empty() {
        return Ok(None);
    }
    Ok(Some(write_roar(path, bitmap)?))
}
