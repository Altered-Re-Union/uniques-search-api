use std::collections::BTreeMap;

use index_core::card::LocaleText;
use index_core::idgd_catalog::{IdGdCatalog, IdGdCatalogEntry};
use index_core::keyword_catalog::KeywordCatalog;
use std::io::Write;

use anyhow::Context;
use axum::body::Bytes;
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::http::api::effect_text::format_effect_part_translations;

use super::models::{EffectPartWithRegion, EffectsListResponse};

/// Build the effects list from `idgd_catalog.json` entries; `keywords` prints `[CODE]` keywords.
pub fn build_effects_list(catalog: &IdGdCatalog, keywords: &KeywordCatalog) -> EffectsListResponse {
    let mut triggers = Vec::new();
    let mut conditions = Vec::new();
    let mut output = Vec::new();

    for entry in &catalog.entries {
        match entry.element_type.as_str() {
            "TRIGGER" => triggers.push(effect_part_with_region(entry, keywords)),
            "CONDITION" => conditions.push(effect_part_with_region(entry, keywords)),
            "OUTPUT" => output.push(effect_part_with_region(entry, keywords)),
            _ => {}
        }
    }

    triggers.sort_by_key(|e| e.id_gd);
    conditions.sort_by_key(|e| e.id_gd);
    output.sort_by_key(|e| e.id_gd);

    EffectsListResponse {
        triggers,
        conditions,
        output,
    }
}

/// Serialize [`EffectsListResponse`] once at startup for a static endpoint body.
pub fn serialize_effects_list(response: &EffectsListResponse) -> anyhow::Result<Bytes> {
    let bytes = serde_json::to_vec(response).context("serialize effects list")?;
    Ok(Bytes::from(bytes))
}

/// Gzip the serialized effects list once at startup (best compression, the body never changes).
pub fn gzip_effects_body(body: &[u8]) -> anyhow::Result<Bytes> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(body).context("gzip effects list")?;
    Ok(Bytes::from(encoder.finish().context("gzip effects list")?))
}

fn effect_part_with_region(
    entry: &IdGdCatalogEntry,
    keywords: &KeywordCatalog,
) -> EffectPartWithRegion {
    EffectPartWithRegion {
        id_gd: entry.id_gd,
        text: translations_to_text(&entry.translations),
        formatted_text: format_effect_part_translations(
            &entry.translations,
            &entry.element_type,
            keywords,
        ),
        is_echo: entry.is_echo,
        is_main: entry.is_main,
        duplicated_id_gd: entry.duplicated_id_gd.clone(),
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

        let list = build_effects_list(&catalog, &KeywordCatalog::default());
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
    fn formatted_text_sits_next_to_raw_text_for_each_locale() {
        let mut condition = entry(191, "CONDITION", "[]", true, false);
        condition
            .translations
            .insert("fr_FR".to_string(), locale_text("fr_FR", "[]"));
        let mut output = entry(42, "OUTPUT", "[RESUPPLY_LOW].", true, false);
        output
            .translations
            .insert("fr_FR".to_string(), locale_text("fr_FR", "[RESUPPLY_LOW]."));
        let catalog = IdGdCatalog {
            set: "TEST".to_string(),
            entries: vec![condition, output],
        };
        let mut keywords = KeywordCatalog::default();
        keywords.insert("RESUPPLY_LOW", "en_US", "Resupply");
        keywords.insert("RESUPPLY_LOW", "fr_FR", "Ravitaillez");

        let body = serialize_effects_list(&build_effects_list(&catalog, &keywords)).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let condition = &value["conditions"][0];
        assert_eq!(condition["text"]["fr_FR"], "[]");
        assert_eq!(condition["formattedText"]["fr_FR"], "Sans condition");
        assert_eq!(condition["formattedText"]["en_US"], "No condition");

        let output = &value["output"][0];
        assert_eq!(output["text"]["fr_FR"], "[RESUPPLY_LOW].");
        assert_eq!(output["formattedText"]["fr_FR"], "**Ravitaillez**.");
        assert_eq!(output["formattedText"]["en_US"], "**Resupply**.");
    }

    #[test]
    fn serialized_body_is_stable_json() {
        let catalog = IdGdCatalog {
            set: "TEST".to_string(),
            entries: vec![entry(1, "TRIGGER", "{R}", true, false)],
        };
        let list = build_effects_list(&catalog, &KeywordCatalog::default());
        let body = serialize_effects_list(&list).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["triggers"][0]["idGd"], 1);
        assert_eq!(value["triggers"][0]["text"]["en_US"], "{R}");
        assert_eq!(value["triggers"][0]["isMain"], true);
        assert_eq!(value["conditions"].as_array().unwrap().len(), 0);
        assert_eq!(value["output"].as_array().unwrap().len(), 0);
    }
}
