use anyhow::{anyhow, Result};
use index_core::build_bitmap_from_ref_strs;
use roaring::RoaringBitmap;

use crate::index::loader::{SetBitmaps, SET_CORE, SET_COREKS};
use crate::index::UniquesIndex;

use super::schema::{FormatDefinition, FormatMode, FormatsManifestEntry};

pub(crate) fn union_requested_sets(bitmaps: &SetBitmaps, sets: &[String]) -> RoaringBitmap {
    let has_core = sets.iter().any(|s| s == SET_CORE);
    let has_coreks = sets.iter().any(|s| s == SET_COREKS);
    let use_combined = has_core && has_coreks;

    let mut out = RoaringBitmap::new();

    if use_combined {
        if let Some(combined) = &bitmaps.core_and_coreks {
            out = combined.clone();
        } else {
            if let Some(b) = bitmaps.by_set.get(SET_CORE) {
                out |= b.clone();
            }
            if let Some(b) = bitmaps.by_set.get(SET_COREKS) {
                out |= b.clone();
            }
        }
    }

    for code in sets {
        if use_combined && (code == SET_CORE || code == SET_COREKS) {
            continue;
        }
        if let Some(b) = bitmaps.by_set.get(code) {
            out |= b.clone();
        }
    }

    out
}

/// Build a format bitmap; returns `(negated, bitmap)`.
pub fn build_format_bitmap(index: &UniquesIndex, def: &FormatDefinition) -> Result<(bool, RoaringBitmap)> {
    match def.mode().map_err(|e| anyhow!(e))? {
        FormatMode::Include => {
            let refs: Vec<&str> = def.included_refs.iter().map(String::as_str).collect();
            let bitmap = build_bitmap_from_ref_strs(index.catalog(), &refs)?;
            Ok((false, bitmap))
        }
        FormatMode::Exclude => {
            let mut bitmap = RoaringBitmap::new();
            if !def.excluded_sets.is_empty() {
                for code in &def.excluded_sets {
                    if !index.set_bitmaps().by_set.contains_key(code) {
                        return Err(anyhow!("unknown set code '{code}'"));
                    }
                }
                bitmap |= union_requested_sets(index.set_bitmaps(), &def.excluded_sets);
            }
            if !def.excluded_refs.is_empty() {
                let refs: Vec<&str> = def.excluded_refs.iter().map(String::as_str).collect();
                bitmap |= build_bitmap_from_ref_strs(index.catalog(), &refs)?;
            }
            Ok((true, bitmap))
        }
    }
}

pub fn load_single_format(
    index: &UniquesIndex,
    entry: &FormatsManifestEntry,
    file_path: &std::path::Path,
) -> Result<(bool, RoaringBitmap)> {
    let text = std::fs::read_to_string(file_path)
        .map_err(|e| anyhow!("read {}: {e}", file_path.display()))?;
    load_single_format_from_text(index, entry, &text, &file_path.display().to_string())
}

/// Same as [`load_single_format`], but for a format already fetched into memory
/// (e.g. downloaded over HTTP). `label` is used in error messages in place of a file path.
pub fn load_single_format_from_text(
    index: &UniquesIndex,
    entry: &FormatsManifestEntry,
    text: &str,
    label: &str,
) -> Result<(bool, RoaringBitmap)> {
    let def: FormatDefinition =
        serde_json::from_str(text).map_err(|e| anyhow!("parse {label}: {e}"))?;
    def.cross_check_manifest(entry).map_err(|e| anyhow!(e))?;
    def.mode().map_err(|e| anyhow!(e))?;
    build_format_bitmap(index, &def)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Bytes;
    use index_core::catalog::{Catalog, FamilyEntry, FamilySet};
    use index_core::idgd_catalog::IdGdCatalog;
    use index_core::idgd_collapse::IdGdAliasMap;

    use crate::index::loader::{
        build_family_lookup_index, build_family_span_groups, build_name_search_index,
        build_set_bitmaps, FactionsSummary, IndexManifest, StatsSummary, SET_CORE, SET_COREKS,
    };
    use crate::index::UniquesIndex;

    use super::*;

    fn test_index() -> UniquesIndex {
        let catalog = Catalog {
            set: "TEST".to_string(),
            faction_order: vec![],
            families: vec![
                FamilyEntry {
                    start_bit: 0,
                    faction: "AX".to_string(),
                    family_number: "01".to_string(),
                    family_id: "AX_01".to_string(),
                    source_set: Some(SET_CORE.to_string()),
                    max_unique_id: 5,
                    card_count: 5,
                    first_reference: "ALT_CORE_B_AX_01_U_1".to_string(),
                    name: Default::default(),
                    artist: String::new(),
                    card_sub_types: vec![],
                    set: FamilySet {
                        reference: "CORE".to_string(),
                        name: String::new(),
                        code: None,
                    },
                },
                FamilyEntry {
                    start_bit: 5,
                    faction: "BR".to_string(),
                    family_number: "02".to_string(),
                    family_id: "BR_02".to_string(),
                    source_set: Some(SET_COREKS.to_string()),
                    max_unique_id: 3,
                    card_count: 3,
                    first_reference: "ALT_COREKS_B_BR_02_U_1".to_string(),
                    name: Default::default(),
                    artist: String::new(),
                    card_sub_types: vec![],
                    set: FamilySet {
                        reference: "COREKS".to_string(),
                        name: String::new(),
                        code: None,
                    },
                },
            ],
            total_bit_span: 8,
        };
        let set_bitmaps = build_set_bitmaps(&catalog);
        UniquesIndex {
            index_dir: PathBuf::from("/test"),
            catalog: catalog.clone(),
            manifest: IndexManifest {
                version: 1,
                set: "TEST".to_string(),
                kind: None,
                built_at_secs: 0,
                card_count: 8,
                id_gd_count: 0,
                total_bit_span: 8,
                family_count: 2,
            },
            idgd_catalog: IdGdCatalog {
                set: "TEST".to_string(),
                entries: vec![],
            },
            id_gd_aliases: IdGdAliasMap::default(),
            stats_summary: StatsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 8,
                fields: vec![],
            },
            factions_summary: FactionsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 8,
                source: String::new(),
                factions: vec![],
                unknown_count: 0,
                bitmap_dir: String::new(),
            },
            cards: vec![],
            id_gd_whole: BTreeMap::new(),
            id_gd_per_line: BTreeMap::new(),
            stats: BTreeMap::new(),
            factions: BTreeMap::new(),
            set_bitmaps,
            name_search_index: build_name_search_index(&catalog),
            family_lookup_index: build_family_lookup_index(&catalog),
            family_span_groups: build_family_span_groups(&catalog),
            effects_body: Arc::new(Bytes::from_static(b"[]")),
            effects_body_gzip: None,
        }
    }

    #[test]
    fn include_refs_builds_bitmap() {
        let index = test_index();
        let def = FormatDefinition {
            id: "std".into(),
            version: 1,
            included_refs: vec!["ALT_CORE_B_AX_01_U_1".into()],
            ..Default::default()
        };
        let (negated, bmp) = build_format_bitmap(&index, &def).unwrap();
        assert!(!negated);
        assert!(bmp.contains(0));
        assert_eq!(bmp.len(), 1);
    }

    #[test]
    fn exclude_set_unions_spans() {
        let index = test_index();
        let def = FormatDefinition {
            id: "no-core".into(),
            version: 1,
            excluded_sets: vec![SET_CORE.into()],
            ..Default::default()
        };
        let (negated, bmp) = build_format_bitmap(&index, &def).unwrap();
        assert!(negated);
        assert_eq!(bmp.len(), 5);
    }
}
