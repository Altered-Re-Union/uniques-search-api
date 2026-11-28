/// Canonical metadata for each known Altered card set (single source of truth across
/// `index-core` and `uniques-http-api` for display codes, reverse code lookup, and release order).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SetMeta {
    pub reference: &'static str,
    pub display_code: Option<&'static str>,
    pub release_rank: usize,
}

pub const KNOWN_SETS: &[SetMeta] = &[
    SetMeta { reference: "CORE",      display_code: Some("BTG"),    release_rank: 0 },
    SetMeta { reference: "COREKS",    display_code: Some("BTG"),    release_rank: 1 },
    SetMeta { reference: "ALIZE",     display_code: Some("TBF"),    release_rank: 2 },
    SetMeta { reference: "BISE",      display_code: Some("WFM"),    release_rank: 3 },
    SetMeta { reference: "JUDGE",     display_code: None,           release_rank: 4 },
    SetMeta { reference: "WCQ25",     display_code: None,           release_rank: 5 },
    SetMeta { reference: "WCF25",     display_code: None,           release_rank: 6 },
    SetMeta { reference: "WCS25",     display_code: None,           release_rank: 7 },
    SetMeta { reference: "CYCLONE",   display_code: Some("SKY"),    release_rank: 8 },
    SetMeta { reference: "DUSTER",    display_code: Some("SDU"),    release_rank: 9 },
    SetMeta { reference: "DUSTERCB",  display_code: Some("SDUCB"),  release_rank: 10 },
    SetMeta { reference: "DUSTEROP",  display_code: Some("SDUOP"),  release_rank: 11 },
    SetMeta { reference: "DUSTERTOP", display_code: Some("SDUTOP"), release_rank: 12 },
    SetMeta { reference: "TCS3",      display_code: None,           release_rank: 13 },
    SetMeta { reference: "MUSUBI",    display_code: None,           release_rank: 14 },
    SetMeta { reference: "EOLE",      display_code: Some("ROC"),    release_rank: 15 },
    SetMeta { reference: "EOLECB",    display_code: Some("ROCCB"),  release_rank: 16 },
    SetMeta { reference: "EOLEOP",    display_code: Some("ROCOP"),  release_rank: 17 },
    SetMeta { reference: "EOLETOP",   display_code: Some("ROCTOP"), release_rank: 18 },
    SetMeta { reference: "FUGUE",     display_code: Some("NEJ"),    release_rank: 19 },
    SetMeta { reference: "WCS26",     display_code: None,           release_rank: 20 },
];

/// Product display code for a `cardSet.reference` value.
pub fn set_code(reference: &str) -> Option<&'static str> {
    KNOWN_SETS
        .iter()
        .find(|s| s.reference == reference)
        .and_then(|s| s.display_code)
}

/// Release-order sort key `(rank, reference)` for deterministic `setDate` sorting.
pub fn set_release_rank(reference: &str) -> (usize, &str) {
    let rank = KNOWN_SETS
        .iter()
        .find(|s| s.reference == reference)
        .map(|s| s.release_rank)
        .unwrap_or(usize::MAX);
    (rank, reference)
}

/// Reverse lookup from a product display code (e.g. `"BTG"`, `"ROC"`) to its set references
/// (case-insensitive). Returns an empty Vec if `code` is not a known display code.
pub fn references_for_display_code(code: &str) -> Vec<&'static str> {
    let upper = code.trim().to_ascii_uppercase();
    KNOWN_SETS
        .iter()
        .filter(|s| s.display_code == Some(upper.as_str()))
        .map(|s| s.reference)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_references_and_reverse_lookup() {
        assert_eq!(set_code("COREKS"), Some("BTG"));
        assert_eq!(set_code("ALIZE"), Some("TBF"));
        assert_eq!(set_code("UNKNOWN"), None);
        assert_eq!(references_for_display_code("BTG"), vec!["CORE", "COREKS"]);
        assert_eq!(references_for_display_code("nej"), vec!["FUGUE"]);
        assert!(references_for_display_code("UNKNOWN").is_empty());
        assert!(set_release_rank("CORE").0 < set_release_rank("EOLE").0);
        assert_eq!(set_release_rank("CUSTOM").0, usize::MAX);
    }
}
