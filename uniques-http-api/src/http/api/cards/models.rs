use std::collections::BTreeMap;

use serde::Serialize;

use index_core::faction_index::Faction;

// --- Response bodies ---

#[derive(Debug, Serialize)]
pub struct CardsIter {
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct CardFaction {
    pub code: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardSetV2 {
    pub reference: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl From<&index_core::catalog::FamilySet> for CardSetV2 {
    fn from(set: &index_core::catalog::FamilySet) -> Self {
        Self {
            reference: set.reference.clone(),
            name: set.name.clone(),
            code: set.code.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardSubTypeV2 {
    pub reference: String,
    pub name: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardV2 {
    pub reference: String,
    pub name: BTreeMap<String, String>,
    pub artist: String,
    pub set: CardSetV2,
    pub card_sub_types: Vec<CardSubTypeV2>,
    pub main_cost: u8,
    pub recall_cost: u8,
    pub forest_power: u8,
    pub mountain_power: u8,
    pub ocean_power: u8,
    pub faction: CardFaction,
    pub main_effect: BTreeMap<String, String>,
    pub echo_effect: BTreeMap<String, String>,
    #[serde(rename = "debug_bga_trigram", skip_serializing_if = "Option::is_none")]
    pub debug_bga_trigram: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FamilyMatchV2 {
    pub family_id: String,
    pub count: u64,
    pub reference: String,
    pub name: BTreeMap<String, String>,
}

#[derive(Debug, Serialize)]
pub struct CardsResponse {
    pub iter: CardsIter,
    pub cards: Vec<CardV2>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub families: Option<Vec<FamilyMatchV2>>,
}

// --- Query parameters ---

#[derive(Debug, Clone, Copy)]
pub(crate) enum IdGdSelector {
    T,
    C,
    O,
}

impl IdGdSelector {
    pub(crate) fn expected_type(self) -> &'static str {
        match self {
            IdGdSelector::T => "TRIGGER",
            IdGdSelector::C => "CONDITION",
            IdGdSelector::O => "OUTPUT",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum EffectCombineMode {
    #[default]
    And,
    Or,
}

#[derive(Debug, Clone)]
pub(crate) struct EffectSlotFilter {
    pub(crate) index: u32,
    pub(crate) t: Vec<u32>,
    pub(crate) c: Vec<u32>,
    pub(crate) o: Vec<u32>,
    pub(crate) match_count: u8,
}

impl Default for EffectSlotFilter {
    fn default() -> Self {
        Self {
            index: 0,
            t: Vec::new(),
            c: Vec::new(),
            o: Vec::new(),
            match_count: 1,
        }
    }
}

impl EffectSlotFilter {
    pub(crate) fn is_empty(&self) -> bool {
        self.t.is_empty() && self.c.is_empty() && self.o.is_empty()
    }
}

#[derive(Debug, Default, Clone)]
pub(crate) struct AbilityFilters {
    pub(crate) effects: Vec<EffectSlotFilter>,
    pub(crate) effect_mode: EffectCombineMode,
    pub(crate) support_t: Vec<u32>,
    pub(crate) support_c: Vec<u32>,
    pub(crate) support_o: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompareOp {
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CostRelation {
    Equal,
    MainHigher,
    RecallHigher,
}

#[derive(Debug, Clone)]
pub(crate) enum CostPredicate {
    Exact(u8),
    AnyOf(Vec<u8>),
    RangeBounds(Vec<(CompareOp, u8)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SortField {
    MainCost,
    RecallCost,
    ForestPower,
    MountainPower,
    OceanPower,
    SetDate,
    Collector,
    Reference,
    Name(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardsSort {
    pub field: SortField,
    pub dir: SortDir,
}

#[derive(Debug, Clone)]
pub struct CardsRequest {
    pub limit: usize,
    pub cursor: Option<u32>,
    pub sort: Option<CardsSort>,
    pub filters: AbilityFilters,
    pub factions: Vec<Faction>,
    pub sets: Vec<String>,
    pub refs: Vec<String>,
    pub main_cost: Option<CostPredicate>,
    pub recall_cost: Option<CostPredicate>,
    pub forest_power: Option<CostPredicate>,
    pub mountain_power: Option<CostPredicate>,
    pub ocean_power: Option<CostPredicate>,
    pub name: Option<String>,
    pub cost_relation: Option<CostRelation>,
    pub sub_types: Vec<String>,
    pub has_no_effect: Option<bool>,
    pub debug_bga_trigram: bool,
    pub with_families: bool,
    pub format: Option<String>,
    pub collection: Option<String>,
}
