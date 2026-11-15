/// Product display code for a `cardSet.reference` value.
pub fn set_code(reference: &str) -> Option<&'static str> {
    match reference {
        "COREKS" | "CORE" => Some("BTG"),
        "ALIZE" => Some("TBF"),
        "BISE" => Some("WFM"),
        "CYCLONE" => Some("SKY"),
        "DUSTER" => Some("SDU"),
        "DUSTERCB" => Some("SDUCB"),
        "DUSTEROP" => Some("SDUOP"),
        "DUSTERTOP" => Some("SDUTOP"),
        "EOLE" => Some("ROC"),
        "EOLECB" => Some("ROCCB"),
        "EOLEOP" => Some("ROCOP"),
        "EOLETOP" => Some("ROCTOP"),
        "FUGUE" => Some("NEJ"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_references() {
        assert_eq!(set_code("COREKS"), Some("BTG"));
        assert_eq!(set_code("ALIZE"), Some("TBF"));
        assert_eq!(set_code("UNKNOWN"), None);
    }
}
