/// Display name for a two-letter faction code.
pub fn faction_display_name(code: &str) -> Option<&'static str> {
    match code {
        "AX" => Some("Axiom"),
        "BR" => Some("Bravos"),
        "LY" => Some("Lyra"),
        "MU" => Some("Muna"),
        "OR" => Some("Ordis"),
        "YZ" => Some("Yzmir"),
        "NE" => Some("Neutral"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_codes() {
        assert_eq!(faction_display_name("MU"), Some("Muna"));
        assert_eq!(faction_display_name("XX"), None);
    }
}
