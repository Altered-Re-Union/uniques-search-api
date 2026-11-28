use std::collections::{BTreeMap, BTreeSet};

use index_core::card::LocaleText;
use index_core::idgd_catalog::{IdGdCatalog, IdGdCatalogEntry};
use index_core::nonunique::NonUniqueIdGdEntry;
use anyhow::Context;
use axum::body::Bytes;

use super::models::{EffectPartWithRegion, EffectsListResponse};

/// Build the effects list from `idgd_catalog.json` entries (uniques only).
pub fn build_effects_list(catalog: &IdGdCatalog) -> EffectsListResponse {
    build_unified_effects_list(catalog, None, None)
}

/// Build the unified effects list merging unique `idgd_catalog.json` with optional non-unique
/// `nonunique/idgd_catalog.json` entries. Shared canonical `idGd`s merge `is_main`/`is_echo`
/// region flags and `card_types`; non-unique-only `idGd`s are appended and sorted by `id_gd`.
///
/// `nu_card_types_by_raw_id` provides bitmap-derived card types for prebuilt indexes whose
/// `nonunique/idgd_catalog.json` does not yet persist `card_types`.
pub fn build_unified_effects_list(
    unique_catalog: &IdGdCatalog,
    nonunique_entries: Option<&[NonUniqueIdGdEntry]>,
    nu_card_types_by_raw_id: Option<&BTreeMap<u32, Vec<String>>>,
) -> EffectsListResponse {
    let mut triggers: BTreeMap<u32, EffectPartWithRegion> = BTreeMap::new();
    let mut conditions: BTreeMap<u32, EffectPartWithRegion> = BTreeMap::new();
    let mut output: BTreeMap<u32, EffectPartWithRegion> = BTreeMap::new();

    for entry in &unique_catalog.entries {
        let target = match entry.element_type.as_str() {
            "TRIGGER" => &mut triggers,
            "CONDITION" => &mut conditions,
            "OUTPUT" => &mut output,
            _ => continue,
        };
        target.insert(entry.id_gd, effect_part_with_region(entry));
    }

    if let Some(nu_entries) = nonunique_entries {
        let aliases = index_core::idgd_collapse::IdGdAliasMap::from_catalog(unique_catalog);
        for entry in nu_entries {
            let canon_id = aliases.resolve(entry.id_gd);
            let target = match entry.element_type.as_str() {
                "TRIGGER" => &mut triggers,
                "CONDITION" => &mut conditions,
                "OUTPUT" => &mut output,
                _ => continue,
            };
            let entry_types: Vec<String> = if !entry.card_types.is_empty() {
                entry.card_types.clone()
            } else if let Some(derived) = nu_card_types_by_raw_id.and_then(|m| m.get(&entry.id_gd)) {
                derived.clone()
            } else {
                Vec::new()
            };
            if let Some(existing) = target.get_mut(&canon_id) {
                existing.is_main |= entry.is_main;
                existing.is_echo |= entry.is_echo;
                existing.spell_only &= entry.spell_only;
                merge_card_types(&mut existing.card_types, &entry_types);
            } else {
                let mut card_types = Vec::new();
                merge_card_types(&mut card_types, &entry_types);
                target.insert(
                    canon_id,
                    EffectPartWithRegion {
                        id_gd: canon_id,
                        text: entry.translations.clone(),
                        is_echo: entry.is_echo,
                        is_main: entry.is_main,
                        duplicated_id_gd: Vec::new(),
                        spell_only: entry.spell_only,
                        card_types,
                    },
                );
            }
        }
    }

    EffectsListResponse {
        triggers: triggers.into_values().collect(),
        conditions: conditions.into_values().collect(),
        output: output.into_values().collect(),
    }
}

fn merge_card_types(dst: &mut Vec<String>, extra: &[String]) {
    if extra.is_empty() {
        return;
    }
    let mut set: BTreeSet<String> = dst.iter().cloned().collect();
    for ct in extra {
        if !ct.is_empty() {
            set.insert(ct.clone());
        }
    }
    *dst = set.into_iter().collect();
}

/// Serialize [`EffectsListResponse`] once at startup for a static endpoint body.
pub fn serialize_effects_list(response: &EffectsListResponse) -> anyhow::Result<Bytes> {
    let bytes = serde_json::to_vec(response).context("serialize effects list")?;
    Ok(Bytes::from(bytes))
}

fn effect_part_with_region(entry: &IdGdCatalogEntry) -> EffectPartWithRegion {
    EffectPartWithRegion {
        id_gd: entry.id_gd,
        text: translations_to_text(&entry.translations),
        is_echo: entry.is_echo,
        is_main: entry.is_main,
        duplicated_id_gd: entry.duplicated_id_gd.clone(),
        spell_only: false,
        // Every unique card in Altered is a Character.
        card_types: vec!["CHARACTER".to_string()],
    }
}

fn translations_to_text(translations: &BTreeMap<String, LocaleText>) -> BTreeMap<String, String> {
    translations
        .iter()
        .map(|(locale, t)| (locale.clone(), t.text.clone()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use index_core::idgd_catalog::IdGdCatalogEntry;

    fn locale_text(locale: &str, text: &str) -> LocaleText {
        LocaleText {
            locale: locale.to_string(),
            text: text.to_string(),
        }
    }

    fn entry(
        id_gd: u32,
        element_type: &str,
        en: &str,
        is_main: bool,
        is_echo: bool,
    ) -> IdGdCatalogEntry {
        IdGdCatalogEntry {
            id_gd,
            card_count: 1,
            bitmap_bytes: 1,
            bitmap_file: format!("{id_gd}.roar"),
            element_type: element_type.to_string(),
            translations: BTreeMap::from([(
                "en_US".to_string(),
                locale_text("en_US", en),
            )]),
            m1: None,
            m2: None,
            m3: None,
            ec: None,
            is_main,
            is_echo,
            duplicated_id_gd: Vec::new(),
        }
    }

    #[test]
    fn build_groups_by_element_type_and_sorts_by_id() {
        let catalog = IdGdCatalog {
            set: "TEST".to_string(),
            entries: vec![
                entry(10, "OUTPUT", "out", false, false),
                entry(3, "TRIGGER", "tri", true, false),
                entry(7, "CONDITION", "cond", true, true),
            ],
        };

        let list = build_effects_list(&catalog);
        assert_eq!(list.triggers.len(), 1);
        assert_eq!(list.triggers[0].id_gd, 3);
        assert_eq!(list.triggers[0].text.get("en_US").map(String::as_str), Some("tri"));
        assert!(list.triggers[0].is_main);
        assert!(!list.triggers[0].is_echo);

        assert_eq!(list.conditions[0].id_gd, 7);
        assert!(list.conditions[0].is_echo);

        assert_eq!(list.output[0].id_gd, 10);
        assert_eq!(list.output.len(), 1);
    }

    #[test]
    fn serialized_body_is_stable_json() {
        let catalog = IdGdCatalog {
            set: "TEST".to_string(),
            entries: vec![entry(1, "TRIGGER", "{R}", true, false)],
        };
        let list = build_effects_list(&catalog);
        let body = serialize_effects_list(&list).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["triggers"][0]["idGd"], 1);
        assert_eq!(value["triggers"][0]["text"]["en_US"], "{R}");
        assert_eq!(value["triggers"][0]["isMain"], true);
        assert_eq!(value["triggers"][0]["cardTypes"], serde_json::json!(["CHARACTER"]));
        assert_eq!(value["conditions"].as_array().unwrap().len(), 0);
        assert_eq!(value["output"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn unified_list_merges_card_types_across_unique_and_nonunique_with_bitmap_fallback() {
        let unique = IdGdCatalog {
            set: "TEST".to_string(),
            entries: vec![entry(10, "OUTPUT", "shared out", true, false)],
        };
        let nu = vec![
            NonUniqueIdGdEntry {
                id_gd: 10,
                element_type: "OUTPUT".to_string(),
                translations: BTreeMap::from([("en_US".to_string(), "shared out".to_string())]),
                is_main: true,
                is_echo: false,
                main_lines: vec![1],
                has_ec: false,
                spell_only: true,
                card_types: vec!["SPELL".to_string()],
            },
            NonUniqueIdGdEntry {
                id_gd: 20,
                element_type: "OUTPUT".to_string(),
                translations: BTreeMap::from([("en_US".to_string(), "spell only".to_string())]),
                is_main: true,
                is_echo: false,
                main_lines: vec![1],
                has_ec: false,
                spell_only: true,
                card_types: Vec::new(), // simulates prebuilt index lacking persisted card_types
            },
            NonUniqueIdGdEntry {
                id_gd: 30,
                element_type: "OUTPUT".to_string(),
                translations: BTreeMap::from([("en_US".to_string(), "landmark only".to_string())]),
                is_main: true,
                is_echo: false,
                main_lines: vec![1],
                has_ec: false,
                spell_only: false,
                card_types: Vec::new(),
            },
        ];
        let fallback = BTreeMap::from([
            (20, vec!["SPELL".to_string()]),
            (30, vec!["LANDMARK_PERMANENT".to_string()]),
        ]);

        let list = build_unified_effects_list(&unique, Some(&nu), Some(&fallback));
        assert_eq!(list.output.len(), 3);
        assert_eq!(list.output[0].id_gd, 10);
        assert_eq!(list.output[0].card_types, vec!["CHARACTER", "SPELL"]);
        assert!(!list.output[0].spell_only);

        assert_eq!(list.output[1].id_gd, 20);
        assert_eq!(list.output[1].card_types, vec!["SPELL"]);
        assert!(list.output[1].spell_only);

        assert_eq!(list.output[2].id_gd, 30);
        assert_eq!(list.output[2].card_types, vec!["LANDMARK_PERMANENT"]);
        assert!(!list.output[2].spell_only);
    }
}
