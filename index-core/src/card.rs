use crate::bitmap::EffectLine;
use crate::catalog::{FamilyCardSubType, FamilySet};
use crate::profile::BuildProfile;
use crate::set_code;
use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::Instant;

/// Parsed card JSON used for idGd indexing and compact field extraction.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardJson {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub translations: Option<BTreeMap<String, CardLocaleEntry>>,
    #[serde(default)]
    pub illustrator: Option<Illustrator>,
    #[serde(default)]
    pub card_sub_types: Vec<CardSubTypeJson>,
    #[serde(default)]
    pub card_set: Option<CardSetJson>,
    #[serde(default)]
    pub main_faction: Option<MainFaction>,
    #[serde(default)]
    pub card_elements: Vec<CardElement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardLocaleEntry {
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Illustrator {
    #[serde(default)]
    pub nick_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardSubTypeJson {
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub translations: Option<BTreeMap<String, CardLocaleEntry>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardSetJson {
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MainFaction {
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardElement {
    #[serde(default)]
    pub card_element_type: Option<CardElementType>,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub card_effect_displays: Vec<CardEffectDisplay>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardElementType {
    #[serde(default)]
    pub reference: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardEffectDisplay {
    #[serde(default)]
    pub card_effect: Option<CardEffect>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardEffect {
    #[serde(default)]
    pub card_effect_elements: Vec<CardEffectElement>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardEffectElement {
    pub id_gd: u32,
    #[serde(rename = "type")]
    pub element_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub translations: Option<BTreeMap<String, LocaleText>>,
    /// Keywords referenced by this element's text (`[FLEETING]`), with their printed names.
    #[serde(default)]
    pub card_effect_element_displays: Option<Vec<CardEffectElementDisplay>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardEffectElementDisplay {
    #[serde(default)]
    pub card_keyword: Option<CardKeywordJson>,
}

/// `cardKeyword`: `reference` is the code used in effect texts (`FLEETING` in `[FLEETING]`).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardKeywordJson {
    #[serde(default)]
    pub reference: Option<String>,
    #[serde(default)]
    pub translations: Option<BTreeMap<String, CardKeywordLocale>>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CardKeywordLocale {
    /// Printed keyword name in this locale (`Fugace`, `Fleeting`, ...).
    #[serde(default)]
    pub display_web: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocaleText {
    pub locale: String,
    pub text: String,
}

/// Pick translated effect text for a locale with `en_US` then first-available fallback.
pub fn translation_text(map: &BTreeMap<String, LocaleText>, locale: &str) -> String {
    if let Some(t) = map.get(locale) {
        return t.text.clone();
    }
    if let Some(t) = map.get("en_US") {
        return t.text.clone();
    }
    map.values().next().map(|t| t.text.clone()).unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct IdGdOccurrence {
    pub id_gd: u32,
    pub element_type: String,
    pub translations: BTreeMap<String, LocaleText>,
}

/// Per-card read/parse timings in nanoseconds (zero when not measured).
#[derive(Debug, Clone, Copy, Default)]
pub struct CardLoadTimings {
    pub read_ns: u64,
    pub parse_ns: u64,
}

/// Read and parse one card JSON file (once per build pass per card).
pub fn load_card(path: &Path, profile: Option<&mut BuildProfile>) -> Result<CardJson> {
    Ok(load_card_timed(path, profile, false)?.0)
}

/// Like [`load_card`], but returns per-card read/parse timings when `measure` is true.
pub fn load_card_timed(
    path: &Path,
    mut profile: Option<&mut BuildProfile>,
    measure: bool,
) -> Result<(CardJson, CardLoadTimings)> {
    if !measure && profile.is_none() {
        let text =
            fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        let card = serde_json::from_str(&text)
            .with_context(|| format!("parse JSON {}", path.display()))?;
        return Ok((card, CardLoadTimings::default()));
    }

    let t0 = Instant::now();
    let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let read_ns = t0.elapsed().as_nanos() as u64;
    if let Some(p) = profile.as_mut() {
        p.read_ns += read_ns;
        p.bytes_read += text.len() as u64;
    }

    let t1 = Instant::now();
    let card: CardJson = serde_json::from_str(&text)
        .with_context(|| format!("parse JSON {}", path.display()))?;
    let parse_ns = t1.elapsed().as_nanos() as u64;
    if let Some(p) = profile.as_mut() {
        p.parse_ns += parse_ns;
    }

    Ok((
        card,
        CardLoadTimings {
            read_ns: if measure { read_ns } else { 0 },
            parse_ns: if measure { parse_ns } else { 0 },
        },
    ))
}

fn translations_from_element(element: &CardEffectElement) -> BTreeMap<String, LocaleText> {
    if let Some(map) = &element.translations {
        return map.clone();
    }
    let mut map = BTreeMap::new();
    if let Some(text) = &element.text {
        map.insert(
            "en_US".to_string(),
            LocaleText {
                locale: "en_US".to_string(),
                text: text.clone(),
            },
        );
    }
    map
}

/// All `idGd` values on one card, grouped by effect line (`m1`..`m3`, `ec`).
///
/// Walks `MAIN_EFFECT` / `ECHO_EFFECT` elements and their `cardEffectDisplays` the same way
/// effect lines are defined in card JSON (not the compact T/C/O slot projection).
pub fn id_gds_per_effect_line(card: &CardJson) -> Vec<(EffectLine, u32)> {
    let mut out = Vec::new();

    for element in &card.card_elements {
        let elem_type = element
            .card_element_type
            .as_ref()
            .and_then(|t| t.reference.as_deref());

        match elem_type {
            Some("MAIN_EFFECT") => {
                for (group_idx, display) in element.card_effect_displays.iter().take(3).enumerate()
                {
                    let line = match group_idx {
                        0 => EffectLine::M1,
                        1 => EffectLine::M2,
                        2 => EffectLine::M3,
                        _ => continue,
                    };
                    if let Some(effect) = &display.card_effect {
                        for node in &effect.card_effect_elements {
                            out.push((line, node.id_gd));
                        }
                    }
                }
            }
            Some("ECHO_EFFECT") => {
                for display in &element.card_effect_displays {
                    if let Some(effect) = &display.card_effect {
                        for node in &effect.card_effect_elements {
                            out.push((EffectLine::Ec, node.id_gd));
                        }
                    }
                }
            }
            _ => {}
        }
    }

    out
}

/// Unique `idGd` occurrences on one card, with effect text metadata.
pub fn effects_from_card(card: &CardJson) -> Vec<IdGdOccurrence> {
    let mut seen = HashSet::new();
    let mut occurrences = Vec::new();

    for element in &card.card_elements {
        for display in &element.card_effect_displays {
            if let Some(effect) = &display.card_effect {
                for node in &effect.card_effect_elements {
                    if seen.insert(node.id_gd) {
                        occurrences.push(IdGdOccurrence {
                            id_gd: node.id_gd,
                            element_type: node
                                .element_type
                                .clone()
                                .unwrap_or_else(|| "UNKNOWN".to_string()),
                            translations: translations_from_element(node),
                        });
                    }
                }
            }
        }
    }
    occurrences
}

/// Unique `idGd` occurrences on one card (deduped per card).
pub fn parse_card_effects(path: &Path) -> Result<Vec<IdGdOccurrence>> {
    Ok(effects_from_card(&load_card(path, None)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bitmap::EffectLine;

    #[test]
    fn family_metadata_from_fixture() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/card-json/ALT_COREKS_B_AX_06_U_5.json");
        let card = load_card(&path, None).unwrap();
        let meta = family_metadata_from_card(&card);
        assert_eq!(meta.name.get("en_US").map(String::as_str), Some("Ogun"));
        assert_eq!(meta.artist, "Edward Cheekokseang");
        assert_eq!(meta.set.reference, "COREKS");
        assert_eq!(meta.set.code.as_deref(), Some("BTG"));
        assert_eq!(meta.card_sub_types.len(), 2);
        assert_eq!(meta.card_sub_types[0].reference, "ENGINEER");
        assert_eq!(
            meta.card_sub_types[0].name.get("en_US").map(String::as_str),
            Some("Engineer")
        );
    }

    #[test]
    fn id_gds_per_effect_line_reads_all_nodes_on_each_display() {
        let json = r#"{
            "cardElements": [{
                "cardElementType": { "reference": "MAIN_EFFECT" },
                "cardEffectDisplays": [{
                    "cardEffect": {
                        "cardEffectElements": [
                            { "idGd": 24, "type": "TRIGGER" },
                            { "idGd": 191, "type": "CONDITION" }
                        ]
                    }
                }]
            }]
        }"#;
        let card: CardJson = serde_json::from_str(json).unwrap();
        let pairs = id_gds_per_effect_line(&card);
        assert!(pairs.contains(&(EffectLine::M1, 24)));
        assert!(pairs.contains(&(EffectLine::M1, 191)));
    }
}

const CARD_NAME_LOCALES: &[&str] = &["en_US", "fr_FR", "de_DE", "es_ES", "it_IT"];

/// Localized card title for catalog / API (`name` field).
pub fn localized_name_map(card: &CardJson) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(map) = &card.translations {
        for (key, entry) in map {
            if let Some(name) = entry.name.as_deref().filter(|s| !s.is_empty()) {
                out.insert(key.clone(), name.to_string());
            }
        }
    }
    if let Some(fallback) = card.name.as_deref().filter(|s| !s.is_empty()) {
        out.entry("en_US".to_string())
            .or_insert_with(|| fallback.to_string());
    }
    out
}

/// Illustrator display name.
pub fn family_artist(card: &CardJson) -> String {
    card.illustrator
        .as_ref()
        .and_then(|i| i.nick_name.clone())
        .unwrap_or_default()
}

fn localized_subtype_names(sub: &CardSubTypeJson) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(map) = &sub.translations {
        for (key, entry) in map {
            if let Some(name) = entry.name.as_deref().filter(|s| !s.is_empty()) {
                out.insert(key.clone(), name.to_string());
            }
        }
    }
    if let Some(fallback) = sub.name.as_deref().filter(|s| !s.is_empty()) {
        out.entry("en_US".to_string())
            .or_insert_with(|| fallback.to_string());
    }
    for locale in CARD_NAME_LOCALES {
        if !out.contains_key(*locale) {
            if let Some(en) = out.get("en_US") {
                out.insert(locale.to_string(), en.clone());
            }
        }
    }
    out
}

/// Subtype list for catalog / API.
pub fn family_card_sub_types(card: &CardJson) -> Vec<FamilyCardSubType> {
    card.card_sub_types
        .iter()
        .filter_map(|sub| {
            let reference = sub.reference.as_deref()?.to_string();
            let name = localized_subtype_names(sub);
            if name.is_empty() {
                return None;
            }
            Some(FamilyCardSubType { reference, name })
        })
        .collect()
}

/// Set metadata for catalog / API.
pub fn family_set(card: &CardJson) -> FamilySet {
    let reference = card
        .card_set
        .as_ref()
        .and_then(|s| s.reference.clone())
        .unwrap_or_default();
    let name = card
        .card_set
        .as_ref()
        .and_then(|s| s.name.clone())
        .unwrap_or_default();
    let code = set_code::set_code(&reference).map(str::to_string);
    FamilySet {
        reference,
        name,
        code,
    }
}

/// All per-family metadata extracted from one card JSON.
pub fn family_metadata_from_card(card: &CardJson) -> crate::catalog::FamilyMetadata {
    crate::catalog::FamilyMetadata {
        name: localized_name_map(card),
        artist: family_artist(card),
        card_sub_types: family_card_sub_types(card),
        set: family_set(card),
    }
}

/// Unique `idGd` values on one card (deduped per card).
pub fn extract_id_gd(path: &Path) -> Result<Vec<u32>> {
    Ok(parse_card_effects(path)?
        .into_iter()
        .map(|o| o.id_gd)
        .collect())
}
