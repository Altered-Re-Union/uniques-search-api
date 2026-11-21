//! Per-print exceptions to the variation (`standard` / `alt-art` / `promo`) that unified search
//! derives from a non-unique reference's product letter (`B` / `A` / `P`).
//!
//! Some cards were only ever printed with a `P` product (e.g. Kuraokami, Libéré des Glaces and
//! Sofia, Premier Avant-poste): they have no `B` print, so their `P` print *is* the base version of
//! the card and must count as `standard`, not as alt-art/promo. The list lives in
//! [`index-core/data/variation_overrides.json`](../../data/variation_overrides.json), compiled into
//! the binary: to add a card, add its references there. It is applied when the HTTP API loads the
//! index (no re-index needed), and only changes the variation: the reference, the `product` field
//! and the `product[]` filter keep the printed product letter.

use serde::Deserialize;
use std::collections::BTreeSet;
use std::sync::OnceLock;

const VARIATION_OVERRIDES_JSON: &str = include_str!("../data/variation_overrides.json");

#[derive(Debug, Deserialize)]
struct VariationOverridesFile {
    standard: Vec<VariationOverride>,
}

#[derive(Debug, Deserialize)]
struct VariationOverride {
    reference: String,
    #[serde(default)]
    #[allow(dead_code)]
    note: String,
}

/// References (uppercased) of the non-standard-product prints that count as the `standard`
/// variation.
pub fn standard_variation_overrides() -> &'static BTreeSet<String> {
    static OVERRIDES: OnceLock<BTreeSet<String>> = OnceLock::new();
    OVERRIDES.get_or_init(|| {
        let file: VariationOverridesFile = serde_json::from_str(VARIATION_OVERRIDES_JSON)
            .expect("index-core/data/variation_overrides.json is valid");
        file.standard
            .into_iter()
            .map(|o| o.reference.trim().to_ascii_uppercase())
            .collect()
    })
}

/// Whether `reference` is listed as a `standard` variation despite its product letter.
pub fn is_standard_variation_override(reference: &str) -> bool {
    standard_variation_overrides().contains(&reference.trim().to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::path::parse_card_reference;

    #[test]
    fn kuraokami_and_sofia_are_standard() {
        for reference in [
            "ALT_ALIZE_P_OR_48_C",
            "ALT_ALIZE_P_OR_48_R1",
            "ALT_ALIZE_P_OR_48_R2",
            "ALT_BISE_P_BR_64_C",
            "ALT_BISE_P_BR_64_R1",
            "ALT_BISE_P_BR_64_R2",
        ] {
            assert!(is_standard_variation_override(reference), "{reference}");
        }
        assert!(is_standard_variation_override("alt_bise_p_br_64_c"));
        assert!(!is_standard_variation_override("ALT_ALIZE_A_OR_48_C"));
    }

    #[test]
    fn every_override_is_a_non_base_non_unique_reference() {
        for reference in standard_variation_overrides() {
            let parts: Vec<&str> = reference.split('_').collect();
            assert!(
                parts.len() == 6 && parts[0] == "ALT" && parts[2] != "B",
                "{reference}: expected ALT_<SET>_<A|P>_<FACTION>_<NUM>_<RARITY>"
            );
            // Unique references (`..._U_<id>`) are always standard: they don't belong here.
            assert!(parse_card_reference(reference).is_err(), "{reference}");
        }
    }
}
