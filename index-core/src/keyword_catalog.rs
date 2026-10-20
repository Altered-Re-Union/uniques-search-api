//! Printed keyword names (`keywords.json`), keyed by the code effect texts embed as `[CODE]`.
//!
//! Collected at build time from each effect element's `cardEffectElementDisplays[].cardKeyword`
//! (`reference` = code, `translations[locale].displayWeb` = printed name), so the names come from
//! the same card data as the effect texts rather than from a hand-maintained table.

use crate::card::CardJson;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// File name of the keyword catalog inside an index directory.
pub const KEYWORDS_FILE: &str = "keywords.json";

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeywordCatalog {
    /// `code -> locale -> printed name`, e.g. `FLEETING -> fr_FR -> Fugace`.
    pub keywords: BTreeMap<String, BTreeMap<String, String>>,
}

impl KeywordCatalog {
    pub fn is_empty(&self) -> bool {
        self.keywords.is_empty()
    }

    /// Printed name of `code` in `locale`, `None` when the code or locale is unknown.
    pub fn name(&self, code: &str, locale: &str) -> Option<&str> {
        self.keywords.get(code)?.get(locale).map(String::as_str)
    }

    /// Record a printed name. The first non-empty name seen for a `(code, locale)` pair wins.
    pub fn insert(&mut self, code: &str, locale: &str, name: &str) {
        let (code, name) = (code.trim(), name.trim());
        if code.is_empty() || name.is_empty() {
            return;
        }
        self.keywords
            .entry(code.to_string())
            .or_default()
            .entry(locale.to_string())
            .or_insert_with(|| name.to_string());
    }

    /// Record every keyword referenced by the effect elements of `card`.
    pub fn record_card(&mut self, card: &CardJson) {
        let elements = card
            .card_elements
            .iter()
            .flat_map(|e| &e.card_effect_displays)
            .filter_map(|d| d.card_effect.as_ref())
            .flat_map(|e| &e.card_effect_elements);
        for element in elements {
            let displays = element.card_effect_element_displays.iter().flatten();
            for keyword in displays.filter_map(|d| d.card_keyword.as_ref()) {
                let Some(code) = keyword.reference.as_deref() else {
                    continue;
                };
                for (locale, t) in keyword.translations.iter().flatten() {
                    if let Some(name) = t.display_web.as_deref() {
                        self.insert(code, locale, name);
                    }
                }
            }
        }
    }

    /// Add the names of `other` that are not already known.
    pub fn merge_from(&mut self, other: &KeywordCatalog) {
        for (code, names) in &other.keywords {
            for (locale, name) in names {
                self.insert(code, locale, name);
            }
        }
    }

    /// Write `keywords.json` into `dir` (nothing when empty).
    pub fn save_in(&self, dir: &Path) -> Result<()> {
        if self.is_empty() {
            return Ok(());
        }
        let path = dir.join(KEYWORDS_FILE);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, text).with_context(|| format!("write {}", path.display()))
    }

    /// Read `keywords.json` from `dir`; empty catalog when the file is absent (older indexes).
    pub fn load_from_dir(dir: &Path) -> Result<Self> {
        let path = dir.join(KEYWORDS_FILE);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text =
            std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::load_card;

    fn fixture(name: &str) -> CardJson {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/card-json")
            .join(name);
        load_card(&path, None).unwrap()
    }

    #[test]
    fn record_card_collects_printed_names_per_locale() {
        let mut catalog = KeywordCatalog::default();
        catalog.record_card(&fixture("ALT_COREKS_B_AX_06_U_5.json"));
        assert_eq!(catalog.name("FLEETING", "fr_FR"), Some("Fugace"));
        assert_eq!(catalog.name("FLEETING", "en_US"), Some("Fleeting"));
        assert_eq!(catalog.name("FLEETING", "es_ES"), Some("Fugacidad"));
        assert_eq!(catalog.name("FLEETING", "de_DE"), Some("Vergänglich"));
        assert_eq!(catalog.name("FLEETING", "it_IT"), Some("Fugace"));
    }

    #[test]
    fn record_card_skips_the_empty_condition_keyword() {
        let mut catalog = KeywordCatalog::default();
        catalog.record_card(&fixture("ALT_COREKS_B_OR_16_U_6.json"));
        assert!(catalog.is_empty());
    }

    #[test]
    fn merge_keeps_existing_names_and_adds_missing_ones() {
        let mut a = KeywordCatalog::default();
        a.insert("ETERNAL", "en_US", "Eternal");
        let mut b = KeywordCatalog::default();
        b.insert("ETERNAL", "en_US", "Other");
        b.insert("ETERNAL", "fr_FR", "Éternel");
        b.insert("FLEETING", "en_US", "Fleeting");
        a.merge_from(&b);
        assert_eq!(a.name("ETERNAL", "en_US"), Some("Eternal"));
        assert_eq!(a.name("ETERNAL", "fr_FR"), Some("Éternel"));
        assert_eq!(a.name("FLEETING", "en_US"), Some("Fleeting"));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            KeywordCatalog::load_from_dir(dir.path())
                .unwrap()
                .is_empty()
        );
        let mut catalog = KeywordCatalog::default();
        catalog.insert("RESUPPLY_LOW", "fr_FR", "Ravitaillez");
        catalog.save_in(dir.path()).unwrap();
        assert_eq!(KeywordCatalog::load_from_dir(dir.path()).unwrap(), catalog);
    }
}
