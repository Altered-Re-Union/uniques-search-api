use std::collections::BTreeMap;

use serde::Serialize;

use crate::http::api::effect_text::FormattedText;

// --- Response bodies ---

/// `GET /api/v2/effects` response body (see `docs/api-spec.md`).
#[derive(Debug, Clone, Serialize)]
pub struct EffectsListResponse {
    pub triggers: Vec<EffectPartWithRegion>,
    pub conditions: Vec<EffectPartWithRegion>,
    pub output: Vec<EffectPartWithRegion>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectPartWithRegion {
    pub id_gd: u32,
    pub text: BTreeMap<String, String>,
    /// Readable version of `text` in Markdown, per locale (see `effect_text::format_effect_part`).
    pub formatted_text: BTreeMap<String, FormattedText>,
    pub is_echo: bool,
    pub is_main: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub duplicated_id_gd: Vec<u32>,
}

/// `GET /api/v2/effects/filtered` response body.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectsFilteredResponse {
    /// Echo of the `editing=<part>:<slot>` request param.
    pub editing: String,
    /// idGds of the edited part that still yield an existing ability under the current filters.
    pub id_gds: Vec<u32>,
}

// --- Query parameters ---

#[derive(Debug, Clone, Copy)]
pub(crate) enum Part {
    Trigger,
    Condition,
    Output,
}

impl Part {
    pub(crate) fn element_type(self) -> &'static str {
        match self {
            Part::Trigger => "TRIGGER",
            Part::Condition => "CONDITION",
            Part::Output => "OUTPUT",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Region {
    /// A main-effect slot (compacted `effect[N]` index). Searches lines M1/M2/M3.
    Main(u32),
    /// The support/echo slot. Searches line Ec.
    Support,
}
