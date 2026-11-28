//! Canonical on-disk JSON index schemas shared by `index-core` writers (`build`, `merge`,
//! `stat_index`, `faction_index`) and `uniques-http-api` readers (`index::loader`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexManifest {
    pub version: u32,
    pub set: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub built_at_secs: u64,
    pub card_count: u32,
    pub id_gd_count: u64,
    pub total_bit_span: u32,
    pub family_count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsSummary {
    pub version: u32,
    pub set: String,
    pub total_cards_indexed: u32,
    pub fields: Vec<StatFieldSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatFieldSummary {
    pub field: String,
    pub element_reference: String,
    pub counts: BTreeMap<u8, u64>,
    pub bitmap_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactionsSummary {
    pub version: u32,
    pub set: String,
    pub total_cards_indexed: u32,
    pub source: String,
    pub factions: Vec<FactionSummary>,
    pub unknown_count: u64,
    pub bitmap_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FactionSummary {
    pub reference: String,
    pub card_count: u64,
    pub bitmap_file: String,
}
