use anyhow::{anyhow, Result};
use index_core::refs_bitmap::build_bitmaps_from_mixed_ref_strs;
use roaring::RoaringBitmap;

use crate::index::loader::{NonUniqueQueryIndex, SetBitmaps, SET_CORE, SET_COREKS};
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

/// Build format bitmaps for both catalogs; returns `(negated, unique_bitmap, nonunique_bitmap)`.
pub fn build_format_bitmap(
    index: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    def: &FormatDefinition,
) -> Result<(bool, RoaringBitmap, RoaringBitmap)> {
    let nu_catalog = nonunique.map(|nu| &nu.catalog);
    match def.mode().map_err(|e| anyhow!(e))? {
        FormatMode::Include => {
            let refs: Vec<&str> = def.included_refs.iter().map(String::as_str).collect();
            let (unique_bmp, nonunique_bmp) =
                build_bitmaps_from_mixed_ref_strs(index.catalog(), nu_catalog, &refs)?;
            Ok((false, unique_bmp, nonunique_bmp))
        }
        FormatMode::Exclude => {
            let mut unique_bmp = RoaringBitmap::new();
            let mut nonunique_bmp = RoaringBitmap::new();
            if !def.excluded_sets.is_empty() {
                for code in &def.excluded_sets {
                    let in_unique = index.set_bitmaps().by_set.contains_key(code);
                    let in_nonunique = nonunique.is_some_and(|nu| nu.set.contains_key(code));
                    if !in_unique && !in_nonunique {
                        return Err(anyhow!("unknown set code '{code}'"));
                    }
                    if let Some(nu) = nonunique {
                        if let Some(b) = nu.set.get(code) {
                            nonunique_bmp |= b;
                        }
                    }
                }
                unique_bmp |= union_requested_sets(index.set_bitmaps(), &def.excluded_sets);
            }
            if !def.excluded_refs.is_empty() {
                let refs: Vec<&str> = def.excluded_refs.iter().map(String::as_str).collect();
                let (u_refs, nu_refs) =
                    build_bitmaps_from_mixed_ref_strs(index.catalog(), nu_catalog, &refs)?;
                unique_bmp |= u_refs;
                nonunique_bmp |= nu_refs;
            }
            Ok((true, unique_bmp, nonunique_bmp))
        }
    }
}

pub fn load_single_format(
    index: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    entry: &FormatsManifestEntry,
    file_path: &std::path::Path,
) -> Result<(bool, RoaringBitmap, RoaringBitmap)> {
    let text = std::fs::read_to_string(file_path)
        .map_err(|e| anyhow!("read {}: {e}", file_path.display()))?;
    load_single_format_from_text(index, nonunique, entry, &text, &file_path.display().to_string())
}

/// Same as [`load_single_format`], but for a format already fetched into memory
/// (e.g. downloaded over HTTP). `label` is used in error messages in place of a file path.
pub fn load_single_format_from_text(
    index: &UniquesIndex,
    nonunique: Option<&NonUniqueQueryIndex>,
    entry: &FormatsManifestEntry,
    text: &str,
    label: &str,
) -> Result<(bool, RoaringBitmap, RoaringBitmap)> {
    let def: FormatDefinition =
        serde_json::from_str(text).map_err(|e| anyhow!("parse {label}: {e}"))?;
    def.cross_check_manifest(entry).map_err(|e| anyhow!(e))?;
    def.mode().map_err(|e| anyhow!(e))?;
    build_format_bitmap(index, nonunique, &def)
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
        build_collector_number_lookup_index, build_family_lookup_index, build_family_span_groups,
        build_name_search_index, build_set_bitmaps, FactionsSummary, IndexManifest, StatsSummary,
        SET_CORE, SET_COREKS,
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
                    card_family_id: None,
                    collector_family_number: None,
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
                    card_family_id: None,
                    collector_family_number: None,
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
            effect_text_search_keys: Vec::new(),
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
            cost_relations: Default::default(),
            subtype_bitmaps: BTreeMap::new(),
            no_effect_bitmap: Default::default(),
            banned: RoaringBitmap::new(),
            errated: RoaringBitmap::new(),
            suspended: RoaringBitmap::new(),
            name_search_index: build_name_search_index(&catalog),
            family_lookup_index: build_family_lookup_index(&catalog),
            family_span_groups: build_family_span_groups(&catalog),
            family_sort_index: Default::default(),
            collector_number_lookup_index: build_collector_number_lookup_index(&catalog),
            present_cards: RoaringBitmap::from_iter(0..catalog.total_bit_span),
            effects_body: Arc::new(Bytes::from_static(b"[]")),
            artist_credits: Default::default(),
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
        let (negated, bmp, nu_bmp) = build_format_bitmap(&index, None, &def).unwrap();
        assert!(!negated);
        assert!(bmp.contains(0));
        assert_eq!(bmp.len(), 1);
        assert!(nu_bmp.is_empty());
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
        let (negated, bmp, nu_bmp) = build_format_bitmap(&index, None, &def).unwrap();
        assert!(negated);
        assert_eq!(bmp.len(), 5);
        assert!(nu_bmp.is_empty());
    }
}
