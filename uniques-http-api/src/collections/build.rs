use anyhow::Result;
use index_core::build_bitmaps_from_mixed_ref_strs;
use index_core::catalog::Catalog;

use crate::index::NonUniqueQueryIndex;

use super::CollectionBitmaps;

/// Resolves a mixed reference list (unique- and non-unique-shaped references, see
/// `build_bitmaps_from_mixed_ref_strs`) into one bitmap per catalog, then bounds-checks each
/// against its own catalog's card count — same all-or-nothing strictness the single-catalog
/// version had, extended across both.
pub fn build_collection_bitmaps(
    catalog: &Catalog,
    nonunique: Option<&NonUniqueQueryIndex>,
    refs: &[String],
    total_bit_span: u32,
) -> Result<(u64, CollectionBitmaps)> {
    let ref_slices: Vec<&str> = refs.iter().map(String::as_str).collect();
    let (unique, nonunique_bitmap) = build_bitmaps_from_mixed_ref_strs(
        catalog,
        nonunique.map(|nu| &nu.catalog),
        &ref_slices,
    )?;
    index_core::validate_bitmap_span(&unique, total_bit_span)?;
    if let Some(nu) = nonunique {
        index_core::validate_bitmap_span(&nonunique_bitmap, nu.total())?;
    }
    let count = unique.len() + nonunique_bitmap.len();
    Ok((
        count,
        CollectionBitmaps {
            unique,
            nonunique: nonunique_bitmap,
        },
    ))
}
