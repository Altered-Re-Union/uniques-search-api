//! Shared family catalog: name/type/subtypes, once per `CardFamilyId`, usable by both unique and
//! non-unique prints of that family. See `cli-indexer/plans/23-family-catalog.md`.
//!
//! Global (not per-set) data, read straight from CardsData's `CardFamilies.csv` /
//! `CardFamilySubTypes.csv` — the same join `cardsdata.rs` and `nonunique.rs` each already do once
//! per print, but this is the first place that needs the *whole* referential rather than one
//! family looked up at a time.

use crate::csv_util::read_all;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const LOCALES: [&str; 5] = ["en_US", "fr_FR", "es_ES", "de_DE", "it_IT"];

#[derive(Debug, Deserialize)]
struct CardFamilyRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "CardTypeReference")]
    card_type_reference: String,
    #[serde(rename = "Name_en_US")]
    name_en_us: String,
    #[serde(rename = "Name_fr_FR")]
    name_fr_fr: String,
    #[serde(rename = "Name_es_ES")]
    name_es_es: String,
    #[serde(rename = "Name_de_DE")]
    name_de_de: String,
    #[serde(rename = "Name_it_IT")]
    name_it_it: String,
}

#[derive(Debug, Deserialize)]
struct CardFamilySubTypeRow {
    #[serde(rename = "CardFamilyId")]
    card_family_id: i64,
    #[serde(rename = "CardSubTypeReference")]
    card_sub_type_reference: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyCatalogEntry {
    pub id: i64,
    pub name: BTreeMap<String, String>,
    pub card_type: String,
    pub subtypes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyCatalog {
    pub families: Vec<FamilyCatalogEntry>,
}

impl FamilyCatalog {
    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text).with_context(|| format!("write {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }
}

/// Reads `<cardsdata_root>/data/csv/{CardFamilies,CardFamilySubTypes}.csv` into one catalog.
pub fn build_family_catalog(cardsdata_root: &Path) -> Result<FamilyCatalog> {
    let csv_root = cardsdata_root.join("data").join("csv");

    let mut subtypes_by_family: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for row in read_all::<CardFamilySubTypeRow>(&csv_root.join("CardFamilySubTypes.csv"))? {
        subtypes_by_family
            .entry(row.card_family_id)
            .or_default()
            .push(row.card_sub_type_reference);
    }

    let mut families = Vec::new();
    for row in read_all::<CardFamilyRow>(&csv_root.join("CardFamilies.csv"))? {
        let names = [
            &row.name_en_us,
            &row.name_fr_fr,
            &row.name_es_es,
            &row.name_de_de,
            &row.name_it_it,
        ];
        let mut name = BTreeMap::new();
        for (locale, value) in LOCALES.iter().zip(names) {
            if !value.is_empty() {
                name.insert(locale.to_string(), value.clone());
            }
        }
        families.push(FamilyCatalogEntry {
            id: row.id,
            name,
            card_type: row.card_type_reference,
            subtypes: subtypes_by_family.get(&row.id).cloned().unwrap_or_default(),
        });
    }

    Ok(FamilyCatalog { families })
}
