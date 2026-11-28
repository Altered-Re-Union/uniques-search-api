use std::collections::BTreeMap;

use roaring::RoaringBitmap;
use serde::Serialize;

use index_core::faction_index::Faction;

// --- Response bodies ---

#[derive(Debug, Serialize)]
pub struct CardsIter {
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<u32>,
    /// Echoes the request's `page` when page-based pagination was used (mutually exclusive with
    /// `cursor` — see `CardsRequest::page`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardTypeV2 {
    pub reference: String,
    pub name: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardRarityV2 {
    pub reference: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardV2 {
    pub reference: String,
    pub name: BTreeMap<String, String>,
    /// Artist credit as printed on the card (a team credit such as `"A - B"` is kept whole).
    pub artist: String,
    /// Searchable artist names of that credit (its members for a team credit, the credit itself
    /// otherwise): the values `artist[]` and `/api/v2/artists` use. Omitted when `artist` is empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artists: Vec<String>,
    pub set: CardSetV2,
    pub card_sub_types: Vec<CardSubTypeV2>,
    pub main_cost: Option<u8>,
    pub recall_cost: Option<u8>,
    pub forest_power: Option<u8>,
    pub mountain_power: Option<u8>,
    pub ocean_power: Option<u8>,
    pub faction: CardFaction,
    pub main_effect: BTreeMap<String, String>,
    pub echo_effect: BTreeMap<String, String>,
    /// `SET-FAMILY-...` (e.g. `BTG-011-U-5`, `BTG-001-C`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collector_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub card_type: Option<CardTypeV2>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rarity: Option<CardRarityV2>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_serialized: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_banned: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_errated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_suspended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permanent: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legal_formats: Option<Vec<String>>,
    #[serde(rename = "debug_bga_trigram", skip_serializing_if = "Option::is_none")]
    pub debug_bga_trigram: Option<String>,
    /// Structured effect lines (trigger / condition / output idGd elements), main lines first then
    /// echo. Only known for uniques (from `cards.bin`); omitted when empty.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub abilities: Vec<CardAbilityV2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CardAbilityKind {
    Main,
    Echo,
}

/// One effect line of a card: its trigger, condition and output elements (`null` when absent).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardAbilityV2 {
    pub kind: CardAbilityKind,
    /// 1-based line number within `kind` (main: M1..M3 position; echo: always 1).
    pub line: u8,
    pub trigger: Option<CardAbilityElementV2>,
    pub condition: Option<CardAbilityElementV2>,
    pub output: Option<CardAbilityElementV2>,
}

/// An idGd effect element (same id space as `/api/v2/effects` and `effect[N][t|c|o]` filters).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CardAbilityElementV2 {
    pub id_gd: u32,
    pub text: BTreeMap<String, String>,
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

impl CostPredicate {
    /// Evaluate this predicate against any 0..=15 bucket provider, shared by unique and non-unique
    /// query paths.
    pub(crate) fn evaluate(
        &self,
        mut get_bucket: impl FnMut(u8) -> Option<RoaringBitmap>,
    ) -> RoaringBitmap {
        let mut out = RoaringBitmap::new();
        match self {
            CostPredicate::Exact(v) => {
                if let Some(bmp) = get_bucket(*v) {
                    out |= bmp;
                }
            }
            CostPredicate::AnyOf(values) => {
                for &v in values {
                    if let Some(bmp) = get_bucket(v) {
                        out |= bmp;
                    }
                }
            }
            CostPredicate::RangeBounds(bounds) => {
                for candidate in 0u8..=15u8 {
                    let keep = bounds.iter().all(|(op, value)| match op {
                        CompareOp::Gt => candidate > *value,
                        CompareOp::Gte => candidate >= *value,
                        CompareOp::Lt => candidate < *value,
                        CompareOp::Lte => candidate <= *value,
                    });
                    if keep {
                        if let Some(bmp) = get_bucket(candidate) {
                            out |= bmp;
                        }
                    }
                }
            }
        }
        out
    }
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
    /// 1-indexed direct page access (via bitmap rank/select), mutually exclusive with `cursor`.
    /// See `uniques-http-api/plans/20-page-pagination.md`.
    pub page: Option<u32>,
    pub sort: Option<CardsSort>,
    pub filters: AbilityFilters,
    pub factions: Vec<Faction>,
    pub sets: Vec<String>,
    pub refs: Vec<String>,
    pub collector_numbers: Vec<String>,
    pub main_cost: Option<CostPredicate>,
    pub recall_cost: Option<CostPredicate>,
    pub forest_power: Option<CostPredicate>,
    pub mountain_power: Option<CostPredicate>,
    pub ocean_power: Option<CostPredicate>,
    pub name: Option<String>,
    pub cost_relation: Option<CostRelation>,
    pub sub_types: Vec<String>,
    pub has_no_effect: Option<bool>,
    /// `hasZeroPower=true`: keep cards with at least one power (forest, mountain, ocean) at 0.
    /// `false` / absent: no filter.
    pub has_zero_power: bool,
    /// `artist[]`: artist names, OR'd; a card matches when a name is one of the searchable names
    /// of its credit, or the printed credit itself (case-insensitive, trimmed).
    pub artists: Vec<String>,
    /// Unified search box: OR across name substring, exact reference, exact collector number.
    /// Independent of `name`/`refs`/`collector_numbers`, which keep their own AND semantics.
    pub q: Option<String>,
    pub debug_bga_trigram: bool,
    pub with_families: bool,
    pub format: Option<String>,
    pub collection: Option<String>,
}
