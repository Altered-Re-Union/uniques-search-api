use crate::compact::CompactCardFields;
use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Faction {
    Ax,
    Br,
    Ly,
    Mu,
    Or,
    Yz,
}

impl Faction {
    pub const ALL: [Faction; 6] = [
        Faction::Ax,
        Faction::Br,
        Faction::Ly,
        Faction::Mu,
        Faction::Or,
        Faction::Yz,
    ];

    pub fn reference(self) -> &'static str {
        match self {
            Faction::Ax => "AX",
            Faction::Br => "BR",
            Faction::Ly => "LY",
            Faction::Mu => "MU",
            Faction::Or => "OR",
            Faction::Yz => "YZ",
        }
    }

    pub fn from_faction_code(code: u8) -> Option<Faction> {
        match code {
            1 => Some(Faction::Ax),
            2 => Some(Faction::Br),
            3 => Some(Faction::Ly),
            4 => Some(Faction::Mu),
            5 => Some(Faction::Or),
            6 => Some(Faction::Yz),
            _ => None,
        }
    }
}

#[derive(Debug, Default)]
pub struct FactionIndexBuilder {
    buckets: BTreeMap<Faction, RoaringBitmap>,
    unknown_count: u64,
}

impl FactionIndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index by `mainFaction.reference` encoded in `fields.faction_code` (JSON only at extract time).
    pub fn insert(&mut self, card_index: u32, fields: &CompactCardFields) {
        match Faction::from_faction_code(fields.faction_code) {
            Some(faction) => {
                self.buckets
                    .entry(faction)
                    .or_insert_with(RoaringBitmap::new)
                    .insert(card_index);
            }
            None => self.unknown_count += 1,
        }
    }

    pub fn into_index(self) -> FactionIndex {
        FactionIndex {
            buckets: self.buckets,
            unknown_count: self.unknown_count,
        }
    }
}

pub struct FactionIndex {
    buckets: BTreeMap<Faction, RoaringBitmap>,
    unknown_count: u64,
}

impl FactionIndex {
    /// Write non-empty faction bitmaps as `factions/{REFERENCE}.roar`.
    pub fn write_dir(&self, factions_root: &Path) -> Result<()> {
        fs::create_dir_all(factions_root)?;
        for faction in Faction::ALL {
            let Some(bitmap) = self.buckets.get(&faction) else {
                continue;
            };
            if bitmap.is_empty() {
                continue;
            }
            let path = factions_root.join(format!("{}.roar", faction.reference()));
            let mut bytes = Vec::new();
            bitmap
                .serialize_into(&mut bytes)
                .with_context(|| format!("serialize faction {}", faction.reference()))?;
            fs::write(&path, bytes)?;
        }
        Ok(())
    }

    pub fn build_summary(&self, set: &str, total_cards_indexed: u32) -> FactionsSummary {
        let mut factions = Vec::with_capacity(Faction::ALL.len());
        for faction in Faction::ALL {
            let card_count = self
                .buckets
                .get(&faction)
                .map(RoaringBitmap::len)
                .unwrap_or(0);
            factions.push(FactionSummary {
                reference: faction.reference().to_string(),
                card_count,
                bitmap_file: format!("factions/{}.roar", faction.reference()),
            });
        }
        FactionsSummary {
            version: 1,
            set: set.to_string(),
            total_cards_indexed,
            source: "mainFaction.reference".to_string(),
            factions,
            unknown_count: self.unknown_count,
            bitmap_dir: "factions".to_string(),
        }
    }

    pub fn save_summary(summary: &FactionsSummary, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(summary)?;
        fs::write(path, text)?;
        Ok(())
    }
}

pub use crate::schema::{FactionSummary, FactionsSummary};
