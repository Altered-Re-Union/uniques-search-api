use crate::card::{family_metadata_from_card, CardJson};
use crate::path::ParsedCardPath;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub const FACTION_ORDER: [&str; 6] = ["AX", "BR", "LY", "MU", "OR", "YZ"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Catalog {
    pub set: String,
    pub faction_order: Vec<String>,
    pub families: Vec<FamilyEntry>,
    pub total_bit_span: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilySet {
    pub reference: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyCardSubType {
    pub reference: String,
    pub name: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyMetadata {
    pub name: BTreeMap<String, String>,
    pub artist: String,
    pub card_sub_types: Vec<FamilyCardSubType>,
    pub set: FamilySet,
    /// CardsData's canonical `CardFamilyId` — see `card.rs`'s `CardJson::card_family_id` doc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_family_id: Option<i64>,
    /// Family-invariant segment of `collectorNumberFormatted` — see `card.rs`'s
    /// `family_collector_number` doc. Uniques only (see
    /// `cli-indexer/plans/24-collector-number-ingestion.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_family_number: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FamilyEntry {
    pub start_bit: u32,
    pub faction: String,
    pub family_number: String,
    pub family_id: String,
    /// Source SET code for this family when produced by `merge`.
    ///
    /// For normal `build` outputs, this is absent and the catalog `set` is used.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_set: Option<String>,
    pub max_unique_id: u32,
    pub card_count: u32,
    pub first_reference: String,
    pub name: BTreeMap<String, String>,
    pub artist: String,
    pub card_sub_types: Vec<FamilyCardSubType>,
    pub set: FamilySet,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_family_id: Option<i64>,
    /// Family-invariant segment of `collectorNumberFormatted` — see `FamilyMetadata`'s field of the
    /// same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_family_number: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DecodedCard {
    pub reference: String,
    pub unique_id: u32,
    pub family_id: String,
    pub faction: String,
    pub family_number: String,
}

struct CurrentFamily {
    family_id: String,
    faction: String,
    family_number: String,
    start_bit: u32,
    max_unique_id: u32,
    card_count: u32,
    last_unique_id: Option<u32>,
    metadata: FamilyMetadata,
}

pub struct CatalogBuilder {
    set: String,
    families: Vec<FamilyEntry>,
    current: Option<CurrentFamily>,
    next_start_bit: u32,
}

impl CatalogBuilder {
    pub fn new(set: impl Into<String>) -> Self {
        Self {
            set: set.into(),
            families: Vec::new(),
            current: None,
            next_start_bit: 0,
        }
    }

    /// Register a card; returns global `card_index` for bitmap insertion.
    ///
    /// `card` supplies per-family metadata captured from the first card in each family.
    pub fn on_card(&mut self, parsed: &ParsedCardPath, card: &CardJson) -> Result<u32> {
        let family_id = parsed.family_id();
        let family_changed = self
            .current
            .as_ref()
            .map(|c| c.family_id != family_id)
            .unwrap_or(true);

        if family_changed {
            self.finalize_current()?;
            self.start_family(parsed, card);
        }

        let current = self
            .current
            .as_mut()
            .expect("family started after transition");

        if current.last_unique_id == Some(parsed.unique_id) {
            bail!(
                "duplicate UniqueID {} in family {} ({})",
                parsed.unique_id,
                family_id,
                parsed.reference()
            );
        }
        current.last_unique_id = Some(parsed.unique_id);
        current.max_unique_id = current.max_unique_id.max(parsed.unique_id);
        current.card_count += 1;

        Ok(current.start_bit + parsed.unique_id - 1)
    }

    pub fn finalize_last(&mut self) -> Result<()> {
        self.finalize_current()
    }

    pub fn into_catalog(mut self) -> Result<Catalog> {
        self.finalize_current()?;
        let total_bit_span = self.next_start_bit;
        Ok(Catalog {
            set: self.set,
            faction_order: FACTION_ORDER.iter().map(|s| s.to_string()).collect(),
            families: self.families,
            total_bit_span,
        })
    }

    fn finalize_current(&mut self) -> Result<()> {
        if let Some(c) = self.current.take() {
            let first_reference = format!(
                "ALT_{}_B_{}_{}_U_1",
                self.set, c.faction, c.family_number
            );
            self.families.push(FamilyEntry {
                start_bit: c.start_bit,
                faction: c.faction.clone(),
                family_number: c.family_number.clone(),
                family_id: c.family_id,
                source_set: None,
                max_unique_id: c.max_unique_id,
                card_count: c.card_count,
                first_reference,
                name: c.metadata.name,
                artist: c.metadata.artist,
                card_sub_types: c.metadata.card_sub_types,
                set: c.metadata.set,
                card_family_id: c.metadata.card_family_id,
                collector_family_number: c.metadata.collector_family_number,
            });
            self.next_start_bit = self
                .next_start_bit
                .checked_add(c.max_unique_id)
                .expect("total_bit_span overflow");
        }
        Ok(())
    }

    fn start_family(&mut self, parsed: &ParsedCardPath, card: &CardJson) {
        self.current = Some(CurrentFamily {
            family_id: parsed.family_id(),
            faction: parsed.faction.clone(),
            family_number: parsed.family_number.clone(),
            start_bit: self.next_start_bit,
            max_unique_id: 0,
            card_count: 0,
            last_unique_id: None,
            metadata: family_metadata_from_card(card),
        });
    }
}

impl Catalog {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, text)?;
        Ok(())
    }

    /// Resolve a card reference to its global bit index.
    pub fn lookup_bit(&self, parsed: &ParsedCardPath) -> Result<u32> {
        let reference = parsed.reference();
        let matches: Vec<_> = self
            .families
            .iter()
            .filter(|f| {
                f.faction == parsed.faction
                    && f.family_number == parsed.family_number
                    && f.source_set.as_deref().unwrap_or(&self.set) == parsed.set
            })
            .collect();

        let family = match matches.len() {
            0 => bail!("reference not found in catalog: {reference}"),
            1 => matches[0],
            _ => bail!("ambiguous reference in catalog: {reference}"),
        };

        if parsed.unique_id > family.max_unique_id {
            bail!(
                "reference {reference} falls in padding after family {} (max UniqueID {})",
                family.family_id,
                family.max_unique_id
            );
        }

        Ok(family.start_bit + parsed.unique_id - 1)
    }

    pub fn family_for_bit(&self, bit: u32) -> Result<&FamilyEntry> {
        let family = self
            .families
            .iter()
            .rfind(|f| f.start_bit <= bit)
            .ok_or_else(|| anyhow::anyhow!("bit {bit} is below first family"))?;
        let unique_id = bit - family.start_bit + 1;
        if unique_id > family.max_unique_id {
            anyhow::bail!(
                "bit {bit} falls in padding after family {} (max UniqueID {})",
                family.family_id,
                family.max_unique_id
            );
        }
        Ok(family)
    }

    pub fn decode_bit(&self, bit: u32) -> Result<DecodedCard> {
        let family = self.family_for_bit(bit)?;

        let unique_id = bit - family.start_bit + 1;
        let set = family.source_set.as_deref().unwrap_or(&self.set);
        Ok(DecodedCard {
            reference: format!(
                "ALT_{}_B_{}_{}_U_{}",
                set, family.faction, family.family_number, unique_id
            ),
            unique_id,
            family_id: family.family_id.clone(),
            faction: family.faction.clone(),
            family_number: family.family_number.clone(),
        })
    }

    pub fn total_cards_indexed(&self) -> u32 {
        self.families.iter().map(|f| f.card_count).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::parse_card_path;
    use std::path::Path;

    fn empty_card() -> CardJson {
        CardJson {
            name: None,
            translations: None,
            illustrator: None,
            card_sub_types: Vec::new(),
            card_set: None,
            main_faction: None,
            card_elements: Vec::new(),
            is_banned: false,
            is_errated: false,
            is_suspended: false,
            card_family_id: None,
            collector_number_formatted: None,
        }
    }

    #[test]
    fn decode_after_build() -> Result<()> {
        let card = empty_card();
        let mut b = CatalogBuilder::new("COREKS");
        let p1 = parse_card_path(
            Path::new("json/COREKS/AX/04/ALT_COREKS_B_AX_04_U_1.json"),
            "COREKS",
        )?;
        let p2 = parse_card_path(
            Path::new("json/COREKS/AX/04/ALT_COREKS_B_AX_04_U_4200.json"),
            "COREKS",
        )?;
        let idx1 = b.on_card(&p1, &card)?;
        let idx2 = b.on_card(&p2, &card)?;
        assert_eq!(idx1, 0);
        assert_eq!(idx2, 4199);

        let p3 = parse_card_path(
            Path::new("json/COREKS/AX/05/ALT_COREKS_B_AX_05_U_1.json"),
            "COREKS",
        )?;
        let p4 = parse_card_path(
            Path::new("json/COREKS/AX/05/ALT_COREKS_B_AX_05_U_6146.json"),
            "COREKS",
        )?;
        let idx3 = b.on_card(&p3, &card)?;
        let idx4 = b.on_card(&p4, &card)?;
        assert_eq!(idx3, 4200);
        assert_eq!(idx4, 10345);

        let catalog = b.into_catalog()?;
        assert_eq!(catalog.families[0].max_unique_id, 4200);
        assert_eq!(catalog.families[1].max_unique_id, 6146);
        assert_eq!(catalog.total_bit_span, 4200 + 6146);

        let d = catalog.decode_bit(10345)?;
        assert_eq!(d.family_id, "AX_05");
        assert_eq!(d.unique_id, 6146);
        assert_eq!(d.reference, "ALT_COREKS_B_AX_05_U_6146");

        let p = crate::path::parse_card_reference("ALT_COREKS_B_AX_05_U_6146")?;
        let bit = catalog.lookup_bit(&p)?;
        assert_eq!(bit, 10345);
        assert_eq!(catalog.decode_bit(bit)?.reference, "ALT_COREKS_B_AX_05_U_6146");
        Ok(())
    }

    #[test]
    fn family_metadata_stored_on_entry() -> Result<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/card-json/ALT_COREKS_B_AX_06_U_5.json");
        let card = crate::card::load_card(&path, None)?;
        let parsed = parse_card_path(
            &Path::new("json/COREKS/AX/06/ALT_COREKS_B_AX_06_U_5.json"),
            "COREKS",
        )?;
        let mut b = CatalogBuilder::new("COREKS");
        b.on_card(&parsed, &card)?;
        let catalog = b.into_catalog()?;
        let family = &catalog.families[0];
        assert_eq!(family.name.get("en_US").map(String::as_str), Some("Ogun"));
        assert_eq!(family.artist, "Edward Cheekokseang");
        assert_eq!(family.set.code.as_deref(), Some("BTG"));
        assert_eq!(family.card_sub_types.len(), 2);
        Ok(())
    }
}
