use std::collections::BTreeMap;

use index_core::card::LocaleText;
use index_core::idgd_catalog::{IdGdCatalog, IdGdCatalogEntry};
use index_core::nonunique::NonUniqueIdGdEntry;
use anyhow::Context;
use axum::body::Bytes;

use super::models::{EffectPartWithRegion, EffectsListResponse};

/// Build the effects list from `idgd_catalog.json` entries (uniques only).
pub fn build_effects_list(catalog: &IdGdCatalog) -> EffectsListResponse {
    build_unified_effects_list(catalog, None)
}

/// Build the unified effects list merging unique `idgd_catalog.json` with optional non-unique
/// `nonunique/idgd_catalog.json` entries. Shared canonical `idGd`s merge `is_main`/`is_echo`
/// region flags; non-unique-only `idGd`s are appended and sorted by `id_gd`.
pub fn build_unified_effects_list(
    unique_catalog: &IdGdCatalog,
    nonunique_entries: Option<&[NonUniqueIdGdEntry]>,
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
            if let Some(existing) = target.get_mut(&canon_id) {
                existing.is_main |= entry.is_main;
                existing.is_echo |= entry.is_echo;
                existing.spell_only &= entry.spell_only;
            } else {
                target.insert(
                    canon_id,
                    EffectPartWithRegion {
                        id_gd: canon_id,
                        text: entry.translations.clone(),
                        is_echo: entry.is_echo,
                        is_main: entry.is_main,
                        duplicated_id_gd: Vec::new(),
                        spell_only: entry.spell_only,
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
        assert_eq!(value["conditions"].as_array().unwrap().len(), 0);
        assert_eq!(value["output"].as_array().unwrap().len(), 0);
    }
}
