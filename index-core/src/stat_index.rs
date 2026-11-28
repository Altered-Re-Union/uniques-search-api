use crate::compact::CompactCardFields;
use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

const VALUE_BUCKETS: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StatField {
    MainCost,
    RecallCost,
    MountainPower,
    OceanPower,
    ForestPower,
}

impl StatField {
    pub const ALL: [StatField; 5] = [
        StatField::MainCost,
        StatField::RecallCost,
        StatField::MountainPower,
        StatField::OceanPower,
        StatField::ForestPower,
    ];

    pub fn dir_name(self) -> &'static str {
        match self {
            StatField::MainCost => "main_cost",
            StatField::RecallCost => "recall_cost",
            StatField::MountainPower => "mountain_power",
            StatField::OceanPower => "ocean_power",
            StatField::ForestPower => "forest_power",
        }
    }

    pub fn element_reference(self) -> &'static str {
        match self {
            StatField::MainCost => "MAIN_COST",
            StatField::RecallCost => "RECALL_COST",
            StatField::MountainPower => "MOUNTAIN_POWER",
            StatField::OceanPower => "OCEAN_POWER",
            StatField::ForestPower => "FOREST_POWER",
        }
    }

    fn value_from_fields(self, fields: &CompactCardFields) -> u8 {
        match self {
            StatField::MainCost => fields.main_cost,
            StatField::RecallCost => fields.recall_cost,
            StatField::MountainPower => fields.mountain_power,
            StatField::OceanPower => fields.ocean_power,
            StatField::ForestPower => fields.forest_power,
        }
    }
}

#[derive(Debug, Default)]
pub struct StatIndexBuilder {
    buckets: BTreeMap<StatField, [RoaringBitmap; VALUE_BUCKETS]>,
}

impl StatIndexBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, card_index: u32, fields: &CompactCardFields) {
        for field in StatField::ALL {
            let value = field.value_from_fields(fields);
            let slot = value as usize;
            if slot >= VALUE_BUCKETS {
                continue;
            }
            self.buckets
                .entry(field)
                .or_insert_with(empty_buckets)[slot]
                .insert(card_index);
        }
    }

    pub fn into_index(self) -> StatIndex {
        StatIndex {
            buckets: self.buckets,
        }
    }
}

pub struct StatIndex {
    buckets: BTreeMap<StatField, [RoaringBitmap; VALUE_BUCKETS]>,
}

impl StatIndex {
    /// Write non-empty value bitmaps under `stats/<field>/{value:02}.roar`.
    pub fn write_dir(&self, stats_root: &Path) -> Result<()> {
        fs::create_dir_all(stats_root)?;
        for field in StatField::ALL {
            let Some(buckets) = self.buckets.get(&field) else {
                continue;
            };
            let field_dir = stats_root.join(field.dir_name());
            fs::create_dir_all(&field_dir)?;
            for (value, bitmap) in buckets.iter().enumerate() {
                if bitmap.is_empty() {
                    continue;
                }
                let path = field_dir.join(format!("{value:02}.roar"));
                let mut bytes = Vec::new();
                bitmap
                    .serialize_into(&mut bytes)
                    .with_context(|| {
                        format!(
                            "serialize {} value {}",
                            field.element_reference(),
                            value
                        )
                    })?;
                fs::write(&path, bytes)?;
            }
        }
        Ok(())
    }

    pub fn build_summary(&self, set: &str, total_cards_indexed: u32) -> StatsSummary {
        let mut fields = Vec::with_capacity(StatField::ALL.len());
        for field in StatField::ALL {
            let counts = self.counts_for(field);
            fields.push(StatFieldSummary {
                field: field.dir_name().to_string(),
                element_reference: field.element_reference().to_string(),
                counts,
                bitmap_dir: format!("stats/{}", field.dir_name()),
            });
        }
        StatsSummary {
            version: 1,
            set: set.to_string(),
            total_cards_indexed,
            fields,
        }
    }

    fn counts_for(&self, field: StatField) -> BTreeMap<u8, u64> {
        let mut counts = BTreeMap::new();
        if let Some(buckets) = self.buckets.get(&field) {
            for (value, bitmap) in buckets.iter().enumerate() {
                let n = bitmap.len();
                if n > 0 {
                    counts.insert(value as u8, n);
                }
            }
        }
        counts
    }

    pub fn save_summary(summary: &StatsSummary, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(summary)?;
        fs::write(path, text)?;
        Ok(())
    }
}

pub use crate::schema::{StatFieldSummary, StatsSummary};

fn empty_buckets() -> [RoaringBitmap; VALUE_BUCKETS] {
    std::array::from_fn(|_| RoaringBitmap::new())
}
