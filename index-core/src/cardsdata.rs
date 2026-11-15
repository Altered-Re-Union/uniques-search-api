//! Adapter from CardsData's exported CSVs to the existing [`CardJson`] shape.
//!
//! Scope: unique prints only (see [`cli-indexer/plans/15-cardsdata-csv-ingestion.md`]). Builds a
//! [`CardJson`] value per print **in memory** and hands it to the unchanged `CatalogBuilder` /
//! `compact_fields_from_card` pipeline — no JSON is ever written to disk (see decision D2 in
//! `docs/non-unique-refonte-decisions.md`).
//!
//! [`cli-indexer/plans/15-cardsdata-csv-ingestion.md`]: https://github.com/Altered-Re-Union/uniques-search-api/blob/main/cli-indexer/plans/15-cardsdata-csv-ingestion.md

use crate::card::{
    CardEffect, CardEffectDisplay, CardEffectElement, CardElement, CardElementType, CardJson,
    CardLocaleEntry, CardSetJson, CardSubTypeJson, Illustrator, LocaleText, MainFaction,
};
use crate::csv_util::read_all;
use crate::path::{parse_card_reference, sort_key, ParsedCardPath};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

const LOCALES: [&str; 5] = ["en_US", "fr_FR", "es_ES", "de_DE", "it_IT"];

// --- CSV row shapes (only the columns this slice needs; extra CSV columns are ignored) ---

#[derive(Debug, Deserialize)]
struct CardFamilyRow {
    // `CardTypeReference` is intentionally not read here: `CardJson` has no slot for it today.
    // Surfacing card type (needed so e.g. non-CHARACTER filters can skip the unique branch
    // entirely, see D4) is Lot 5 scope, not this ingestion slice.
    #[serde(rename = "Id")]
    id: i64,
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

#[derive(Debug, Deserialize)]
struct CardSubTypeRow {
    #[serde(rename = "Reference")]
    reference: String,
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
struct SetRow {
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "Name_en_US")]
    name_en_us: String,
}

#[derive(Debug, Deserialize)]
struct ArtistRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "NickName")]
    nick_name: String,
}

#[derive(Debug, Deserialize)]
struct EffectFragmentRow {
    #[serde(rename = "IdGd")]
    id_gd: i64,
    #[serde(rename = "Type")]
    fragment_type: String,
    #[serde(rename = "Slot")]
    slot: String,
    #[serde(rename = "Text_en_US")]
    text_en_us: String,
    #[serde(rename = "Text_fr_FR")]
    text_fr_fr: String,
    #[serde(rename = "Text_es_ES")]
    text_es_es: String,
    #[serde(rename = "Text_de_DE")]
    text_de_de: String,
    #[serde(rename = "Text_it_IT")]
    text_it_it: String,
}

#[derive(Debug, Deserialize)]
struct UniquePrintRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "Reference")]
    reference: String,
    #[serde(rename = "CardFamilyId")]
    card_family_id: i64,
    #[serde(rename = "SetReference")]
    set_reference: String,
    /// The print's own current faction — can differ from the faction embedded in `reference`
    /// (a unique's stats/faction are randomized independently of its family's nominal slot).
    #[serde(rename = "FactionReference")]
    faction_reference: String,
    /// Not always populated in CardsData yet — some prints have no artist on record.
    #[serde(rename = "ArtistId", deserialize_with = "csv::invalid_option")]
    artist_id: Option<i64>,
    #[serde(rename = "IsBanned")]
    is_banned: u8,
    #[serde(rename = "IsErrated")]
    is_errated: u8,
    #[serde(rename = "IsSuspended")]
    is_suspended: u8,
    // Kept as raw strings and parsed downstream by the existing `compact_fields_from_card`
    // (same tolerant `str::parse::<u8>()` it already applies to JSON-sourced values).
    #[serde(rename = "MAIN_COST")]
    main_cost: String,
    #[serde(rename = "RECALL_COST")]
    recall_cost: String,
    #[serde(rename = "MOUNTAIN_POWER")]
    mountain_power: String,
    #[serde(rename = "OCEAN_POWER")]
    ocean_power: String,
    #[serde(rename = "FOREST_POWER")]
    forest_power: String,
}

#[derive(Debug, Deserialize)]
struct UniquePrintEffectFragmentRow {
    #[serde(rename = "UniquePrintId")]
    unique_print_id: i64,
    #[serde(rename = "DisplayIndex")]
    display_index: i64,
    #[serde(rename = "TriggerId", deserialize_with = "csv::invalid_option")]
    trigger_id: Option<i64>,
    #[serde(rename = "ConditionId", deserialize_with = "csv::invalid_option")]
    condition_id: Option<i64>,
    #[serde(rename = "EffectId", deserialize_with = "csv::invalid_option")]
    effect_id: Option<i64>,
}

// --- Loaded, joined dataset ---

/// CardsData referentials + one set's unique prints, loaded and indexed for lookups.
///
/// Construct with [`CardsDataSet::load`], then call [`CardsDataSet::unique_cards`] to get
/// `(ParsedCardPath, CardJson)` pairs ready for the existing `CatalogBuilder` /
/// `compact_fields_from_card` pipeline, in the family-contiguous, ascending-`unique_id` order that
/// pipeline requires.
pub struct CardsDataSet {
    families: BTreeMap<i64, CardFamilyRow>,
    family_sub_types: BTreeMap<i64, Vec<String>>,
    sub_types: BTreeMap<String, CardSubTypeRow>,
    sets: BTreeMap<String, SetRow>,
    artists: BTreeMap<i64, ArtistRow>,
    effect_fragments: BTreeMap<i64, EffectFragmentRow>,
    unique_prints: Vec<UniquePrintRow>,
    unique_print_effects: BTreeMap<i64, Vec<UniquePrintEffectFragmentRow>>,
}

impl CardsDataSet {
    /// `root` is a CardsData checkout; `set` selects `data/csv/Unique/<set>/*.csv`.
    pub fn load(root: &Path, set: &str) -> Result<Self> {
        let csv_root = root.join("data").join("csv");

        let families = read_indexed(&csv_root.join("CardFamilies.csv"), |r: &CardFamilyRow| r.id)?;
        let family_sub_types = read_family_sub_types(&csv_root.join("CardFamilySubTypes.csv"))?;
        let sub_types = read_indexed(&csv_root.join("CardSubTypes.csv"), |r: &CardSubTypeRow| {
            r.reference.clone()
        })?;
        let sets = read_indexed(&csv_root.join("Sets.csv"), |r: &SetRow| r.reference.clone())?;
        let artists = read_indexed(&csv_root.join("Artists.csv"), |r: &ArtistRow| r.id)?;
        let effect_fragments = read_indexed(&csv_root.join("EffectFragments.csv"), |r: &EffectFragmentRow| {
            r.id_gd
        })?;

        let set_dir = csv_root.join("Unique").join(set);
        let unique_prints = read_all(&set_dir.join("UniquePrints.csv"))?;
        let unique_print_effects =
            read_unique_print_effects(&set_dir.join("UniquePrintEffectFragments.csv"))?;

        Ok(Self {
            families,
            family_sub_types,
            sub_types,
            sets,
            artists,
            effect_fragments,
            unique_prints,
            unique_print_effects,
        })
    }

    /// All unique prints for this set as `(ParsedCardPath, CardJson)`, sorted the way
    /// `CatalogBuilder::on_card` requires (family-contiguous, ascending `unique_id`).
    ///
    /// A row whose `Reference` doesn't match the `ALT_<SET>_B_<faction>_<family>_U_<id>` pattern
    /// is skipped rather than failing the whole build — mirrors `crawl.rs`'s handling of
    /// non-card files (e.g. CardsData carries a `FOILER` placeholder row, same idea as the
    /// `FOILER` paths the JSON-crawl discovery already filters out).
    pub fn unique_cards(&self) -> Result<Vec<(ParsedCardPath, CardJson)>> {
        let mut out = Vec::with_capacity(self.unique_prints.len());
        for print in &self.unique_prints {
            let Ok(parsed) = parse_card_reference(&print.reference) else {
                continue;
            };
            let card = self.card_json_for_print(print)?;
            out.push((parsed, card));
        }
        out.sort_by_key(|(parsed, _)| sort_key(parsed));
        Ok(out)
    }

    fn card_json_for_print(&self, print: &UniquePrintRow) -> Result<CardJson> {
        let family = self.families.get(&print.card_family_id).with_context(|| {
            format!(
                "unknown CardFamilyId {} for print {}",
                print.card_family_id, print.reference
            )
        })?;
        let illustrator = print
            .artist_id
            .map(|id| {
                self.artists.get(&id).with_context(|| {
                    format!("unknown ArtistId {id} for print {}", print.reference)
                })
            })
            .transpose()?
            .map(|artist| Illustrator {
                nick_name: Some(artist.nick_name.clone()),
            });
        let set = self.sets.get(&print.set_reference).with_context(|| {
            format!(
                "unknown SetReference {} for print {}",
                print.set_reference, print.reference
            )
        })?;

        let mut card_elements = vec![
            stat_element("MAIN_COST", &print.main_cost),
            stat_element("RECALL_COST", &print.recall_cost),
            stat_element("MOUNTAIN_POWER", &print.mountain_power),
            stat_element("OCEAN_POWER", &print.ocean_power),
            stat_element("FOREST_POWER", &print.forest_power),
        ];
        if let Some(el) = self.effect_element_for_slot(print.id, "MAIN_EFFECT")? {
            card_elements.push(el);
        }
        if let Some(el) = self.effect_element_for_slot(print.id, "ECHO_EFFECT")? {
            card_elements.push(el);
        }

        Ok(CardJson {
            name: None,
            translations: Some(family_translations(family)),
            illustrator,
            card_sub_types: self.card_sub_types_for_family(print.card_family_id),
            card_set: Some(CardSetJson {
                reference: Some(set.reference.clone()),
                name: Some(set.name_en_us.clone()),
            }),
            main_faction: Some(MainFaction {
                reference: Some(print.faction_reference.clone()),
            }),
            card_elements,
            is_banned: print.is_banned != 0,
            is_errated: print.is_errated != 0,
            is_suspended: print.is_suspended != 0,
            card_family_id: Some(print.card_family_id),
            // Deferred: CardsData's collector-number source column isn't confirmed yet (CardsData
            // isn't vendored in this repo). See cli-indexer/plans/24-collector-number-ingestion.md.
            collector_number_formatted: None,
        })
    }

    /// Rows for `unique_print_id` whose Trigger resolves to `slot` (`MAIN_EFFECT` or
    /// `ECHO_EFFECT`), ordered by `DisplayIndex`.
    ///
    /// Per D5, `Slot` lives on the Trigger fragment, not on the row itself — a row with no
    /// `TriggerId` can't be assigned a slot and is skipped (see open question Q1 in the decision
    /// log: this assumes every row has a Trigger, unverified against the full dataset yet).
    fn effect_rows_for_slot(
        &self,
        unique_print_id: i64,
        slot: &str,
    ) -> Result<Vec<&UniquePrintEffectFragmentRow>> {
        let Some(rows) = self.unique_print_effects.get(&unique_print_id) else {
            return Ok(Vec::new());
        };
        let mut matched = Vec::new();
        for row in rows {
            let Some(trigger_id) = row.trigger_id else {
                continue;
            };
            let fragment = self.effect_fragments.get(&trigger_id).with_context(|| {
                format!("unknown TriggerId {trigger_id} on unique print {unique_print_id}")
            })?;
            if fragment.slot == slot {
                matched.push(row);
            }
        }
        matched.sort_by_key(|r| r.display_index);
        Ok(matched)
    }

    fn effect_element_for_slot(
        &self,
        unique_print_id: i64,
        slot: &str,
    ) -> Result<Option<CardElement>> {
        let rows = self.effect_rows_for_slot(unique_print_id, slot)?;
        if rows.is_empty() {
            return Ok(None);
        }
        // Mirrors `compact_fields_from_card`: MAIN_EFFECT keeps at most 3 displays (M1/M2/M3);
        // ECHO_EFFECT has no such cap there (first non-zero trigger/condition/output wins).
        let take = if slot == "MAIN_EFFECT" { 3 } else { rows.len() };
        let mut displays = Vec::with_capacity(take.min(rows.len()));
        for row in rows.into_iter().take(take) {
            displays.push(CardEffectDisplay {
                card_effect: Some(CardEffect {
                    card_effect_elements: self.effect_elements_for_row(row)?,
                }),
            });
        }
        Ok(Some(CardElement {
            card_element_type: Some(CardElementType {
                reference: Some(slot.to_string()),
            }),
            value: None,
            card_effect_displays: displays,
        }))
    }

    fn effect_elements_for_row(
        &self,
        row: &UniquePrintEffectFragmentRow,
    ) -> Result<Vec<CardEffectElement>> {
        let mut elements = Vec::new();
        for (id, kind, expected_type) in [
            (row.trigger_id, "TRIGGER", "Trigger"),
            (row.condition_id, "CONDITION", "Condition"),
            (row.effect_id, "OUTPUT", "Output"),
        ] {
            let Some(id) = id else { continue };
            let fragment = self
                .effect_fragments
                .get(&id)
                .with_context(|| format!("unknown {kind} fragment id {id}"))?;
            if fragment.fragment_type != expected_type {
                bail!(
                    "fragment {id} sits in the {kind} column but EffectFragments.Type is {:?} \
                     (expected {expected_type:?})",
                    fragment.fragment_type
                );
            }
            let id_gd: u32 = id.try_into().with_context(|| {
                format!(
                    "negative idGd {id} on a unique print — unexpected outside the freeform \
                     non-unique effect space (see decision D6)"
                )
            })?;
            elements.push(CardEffectElement {
                id_gd,
                element_type: Some(kind.to_string()),
                text: None,
                translations: Some(fragment_translations(fragment)),
            });
        }
        Ok(elements)
    }

    fn card_sub_types_for_family(&self, family_id: i64) -> Vec<CardSubTypeJson> {
        let Some(refs) = self.family_sub_types.get(&family_id) else {
            return Vec::new();
        };
        refs.iter()
            .filter_map(|reference| {
                let sub = self.sub_types.get(reference)?;
                Some(CardSubTypeJson {
                    reference: Some(sub.reference.clone()),
                    name: Some(sub.name_en_us.clone()),
                    translations: Some(sub_type_translations(sub)),
                })
            })
            .collect()
    }
}

fn stat_element(kind: &str, raw_value: &str) -> CardElement {
    CardElement {
        card_element_type: Some(CardElementType {
            reference: Some(kind.to_string()),
        }),
        value: Some(raw_value.to_string()),
        card_effect_displays: Vec::new(),
    }
}

fn family_translations(family: &CardFamilyRow) -> BTreeMap<String, CardLocaleEntry> {
    let names = [
        &family.name_en_us,
        &family.name_fr_fr,
        &family.name_es_es,
        &family.name_de_de,
        &family.name_it_it,
    ];
    locale_map(names, |locale, text| CardLocaleEntry {
        locale: Some(locale.to_string()),
        name: Some(text.to_string()),
    })
}

fn sub_type_translations(sub: &CardSubTypeRow) -> BTreeMap<String, CardLocaleEntry> {
    let names = [
        &sub.name_en_us,
        &sub.name_fr_fr,
        &sub.name_es_es,
        &sub.name_de_de,
        &sub.name_it_it,
    ];
    locale_map(names, |locale, text| CardLocaleEntry {
        locale: Some(locale.to_string()),
        name: Some(text.to_string()),
    })
}

fn fragment_translations(fragment: &EffectFragmentRow) -> BTreeMap<String, LocaleText> {
    let texts = [
        &fragment.text_en_us,
        &fragment.text_fr_fr,
        &fragment.text_es_es,
        &fragment.text_de_de,
        &fragment.text_it_it,
    ];
    locale_map(texts, |locale, text| LocaleText {
        locale: locale.to_string(),
        text: text.to_string(),
    })
}

fn locale_map<V>(values: [&String; 5], make: impl Fn(&str, &str) -> V) -> BTreeMap<String, V> {
    let mut map = BTreeMap::new();
    for (locale, value) in LOCALES.iter().zip(values) {
        if !value.is_empty() {
            map.insert(locale.to_string(), make(locale, value));
        }
    }
    map
}

// --- CSV loading helpers ---

fn read_indexed<T, K, F>(path: &Path, key_fn: F) -> Result<BTreeMap<K, T>>
where
    T: serde::de::DeserializeOwned,
    K: Ord,
    F: Fn(&T) -> K,
{
    Ok(read_all::<T>(path)?
        .into_iter()
        .map(|row| (key_fn(&row), row))
        .collect())
}

fn read_family_sub_types(path: &Path) -> Result<BTreeMap<i64, Vec<String>>> {
    let rows: Vec<CardFamilySubTypeRow> = read_all(path)?;
    let mut out: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for row in rows {
        out.entry(row.card_family_id)
            .or_default()
            .push(row.card_sub_type_reference);
    }
    Ok(out)
}

fn read_unique_print_effects(
    path: &Path,
) -> Result<BTreeMap<i64, Vec<UniquePrintEffectFragmentRow>>> {
    let rows: Vec<UniquePrintEffectFragmentRow> = read_all(path)?;
    let mut out: BTreeMap<i64, Vec<UniquePrintEffectFragmentRow>> = BTreeMap::new();
    for row in rows {
        out.entry(row.unique_print_id).or_default().push(row);
    }
    Ok(out)
}
