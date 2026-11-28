//! Word-based text search shared by `q`, `name` and `effectText`.
//!
//! Indexed text and queries go through the same [`search_key`] normalization (diacritics
//! stripped, lowercased, punctuation and non-breaking spaces turned into single regular spaces,
//! `{J}`-style symbols kept as their own words). A query then matches a text when **every**
//! term of the [`SearchQuery`] is a substring of that text's key, so `piochez carte` finds
//! "Piochez une carte." and `pioch` finds "piochez". Terms wrapped in double quotes are kept as
//! one phrase (`"une carte"`).

use unicode_normalization::UnicodeNormalization;

/// Normalized form used for text search, on both the indexed side and the query side.
///
/// - diacritics are stripped and letters lowercased (`Élémentaire` → `elementaire`);
/// - every run of characters that is neither a letter nor a digit (punctuation, apostrophes,
///   dashes, regular / non-breaking spaces) becomes one regular space;
/// - `{` and `}` are kept, so game symbols such as `{J}` stay distinct words (`{j}`) instead of
///   collapsing into a bare letter that would match almost any text;
/// - leading/trailing spaces are trimmed.
pub fn search_key(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text
        .nfd()
        .flat_map(char::to_lowercase)
        .filter(|c| !unicode_normalization::char::is_combining_mark(*c))
    {
        if c.is_alphanumeric() || c == '{' || c == '}' {
            if pending_space && !out.is_empty() {
                out.push(' ');
            }
            pending_space = false;
            out.push(c);
        } else {
            pending_space = true;
        }
    }
    out
}

/// A parsed search query: a list of normalized terms that must all be found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    terms: Vec<String>,
}

impl SearchQuery {
    /// Splits `raw` into terms: each double-quoted section is one phrase term, every other word
    /// is its own term. Terms are normalized with [`search_key`]; empty ones are dropped.
    /// Returns `None` when nothing searchable is left (empty, whitespace or punctuation only).
    pub fn parse(raw: &str) -> Option<Self> {
        let mut terms = Vec::new();
        for (i, part) in raw.split('"').enumerate() {
            let key = search_key(part);
            if key.is_empty() {
                continue;
            }
            if i % 2 == 1 {
                // Inside quotes: keep the phrase as a single term.
                terms.push(key);
            } else {
                terms.extend(key.split(' ').map(str::to_string));
            }
        }
        terms.sort();
        terms.dedup();
        (!terms.is_empty()).then_some(Self { terms })
    }

    pub fn terms(&self) -> &[String] {
        &self.terms
    }

    /// `true` when every term is a substring of `key` (a value produced by [`search_key`]).
    pub fn matches(&self, key: &str) -> bool {
        self.terms.iter().all(|term| key.contains(term.as_str()))
    }

    /// `true` when every term is a substring of at least one of `keys` (e.g. a card's name and
    /// effect text in one locale).
    pub fn matches_any_field(&self, keys: &[&str]) -> bool {
        self.terms
            .iter()
            .all(|term| keys.iter().any(|key| key.contains(term.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_key_strips_accents_case_and_punctuation() {
        assert_eq!(search_key("Élémentaire de Kélon"), "elementaire de kelon");
        assert_eq!(search_key("Piochez une carte."), "piochez une carte");
        assert_eq!(search_key("  l'adversaire — défausse !  "), "l adversaire defausse");
    }

    #[test]
    fn search_key_turns_non_breaking_spaces_into_spaces() {
        assert_eq!(search_key("gagne 1\u{00A0}boost"), "gagne 1 boost");
        assert_eq!(search_key("Piochez une carte\u{202F}:"), "piochez une carte");
    }

    #[test]
    fn search_key_keeps_symbols_as_words() {
        assert_eq!(search_key("{J} Piochez une carte."), "{j} piochez une carte");
        assert_eq!(search_key("moins de\u{00A0}{2}."), "moins de {2}");
    }

    #[test]
    fn parse_splits_words_and_keeps_quoted_phrases() {
        let q = SearchQuery::parse("Piochez \"une  CARTE\" réserve").unwrap();
        assert_eq!(q.terms(), ["piochez", "reserve", "une carte"]);
    }

    #[test]
    fn parse_returns_none_without_searchable_text() {
        assert_eq!(SearchQuery::parse(""), None);
        assert_eq!(SearchQuery::parse("   "), None);
        assert_eq!(SearchQuery::parse(" , ; \"\" "), None);
    }

    #[test]
    fn matches_requires_every_term_anywhere_in_the_text() {
        let text = search_key("Piochez une carte, puis défaussez une carte de votre main.");
        assert!(SearchQuery::parse("piochez carte").unwrap().matches(&text));
        assert!(SearchQuery::parse("main pioch").unwrap().matches(&text));
        assert!(SearchQuery::parse("\"une carte\"").unwrap().matches(&text));
        assert!(!SearchQuery::parse("piochez boost").unwrap().matches(&text));
        assert!(!SearchQuery::parse("\"piochez carte\"").unwrap().matches(&text));
    }

    #[test]
    fn symbol_terms_only_match_symbols() {
        let with_symbol = search_key("{J} Je gagne 1 boost.");
        let without_symbol = search_key("Je gagne 1 boost.");
        let q = SearchQuery::parse("{J} boost").unwrap();
        assert!(q.matches(&with_symbol));
        assert!(!q.matches(&without_symbol));
    }

    #[test]
    fn matches_any_field_spreads_terms_over_fields() {
        let name = search_key("Élémentaire de Kélon");
        let effect = search_key("Piochez une carte.");
        let q = SearchQuery::parse("kelon piochez").unwrap();
        assert!(q.matches_any_field(&[&name, &effect]));
        assert!(!q.matches(&name));
    }
}
