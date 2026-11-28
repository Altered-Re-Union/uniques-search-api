//! Shared card effect-text & keyword semantics used across both the build-time non-unique
//! indexer (`index_core::nonunique`) and the runtime server (`uniques-http-api`).
//!
//! Centralizes bracket-token `[KEYWORD]` parsing, keyword canonicalization, localized display
//! expansion, and rules-reminder detection/stripping so unique and non-unique cards never drift.

use std::collections::{BTreeMap, BTreeSet};

/// Canonicalize a keyword reference or bracket token (e.g. `FLEETING_CHAR` -> `FLEETING`,
/// `AFTER YOU` -> `AFTER_YOU`, `TOUGH 1` -> `TOUGH`, `RESUPPLY_INF` -> `RESUPPLY`).
/// Returns `None` for empty strings and invisible marker tokens (`BB`).
pub fn canonical_keyword(raw: &str) -> Option<String> {
    let k = raw.trim().replace(' ', "_").to_ascii_uppercase();
    if k.is_empty() || k == "BB" {
        return None;
    }
    let mapped = match k.as_str() {
        s if s.starts_with("ANCHORED") => "ANCHORED",
        s if s.starts_with("ASLEEP") => "ASLEEP",
        s if s.starts_with("ASCEND") || s == "DUE_TO_ASCENSION" => "ASCEND",
        s if s.starts_with("BOOSTED") => "BOOSTED",
        s if s.starts_with("COOLDOWN") => "COOLDOWN",
        s if s.starts_with("DEFENDER") => "DEFENDER",
        s if s.starts_with("ETERNAL") => "ETERNAL",
        s if s.starts_with("FLEETING") => "FLEETING",
        s if s.starts_with("GIGANTIC") => "GIGANTIC",
        s if s.starts_with("RESUPPL") || s.starts_with("EXHAUSTED_RESUPPL") => "RESUPPLY",
        s if s.starts_with("SABOTAGE") => "SABOTAGE",
        s if s.starts_with("SCOUT") => "SCOUT",
        s if s.starts_with("SEASONED") => "SEASONED",
        s if s.starts_with("TOUGH") => "TOUGH",
        other => other,
    };
    Some(mapped.to_string())
}

/// True when `token` inside `[...]` looks like an Altered keyword code (uppercase ASCII letters,
/// digits, underscores, or internal spaces such as `AFTER YOU` / `TOUGH 1`).
pub fn is_keyword_token(token: &str) -> bool {
    let trimmed = token.trim();
    !trimmed.is_empty()
        && trimmed
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_' || b == b' ')
        && trimmed.bytes().any(|b| b.is_ascii_uppercase())
}

/// Iterate over every raw `[TOKEN]` substring inside `text` that satisfies [`is_keyword_token`].
pub fn extract_bracket_tokens(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            break;
        };
        let token = &after[..close];
        if is_keyword_token(token) {
            out.push(token);
        }
        rest = &after[close + 1..];
    }
    out
}

/// Extract all canonical keyword codes referenced via `[KEYWORD]` tokens in `text`.
pub fn extract_canonical_keywords(text: &str, out: &mut BTreeSet<String>) {
    for token in extract_bracket_tokens(text) {
        if let Some(canon) = canonical_keyword(token) {
            out.insert(canon);
        }
    }
}

/// Replace `[KEYWORD]` / `$[KEYWORD]` markers in `text` with their localized display text from
/// `keywords` (falling back to `en_US`, dropping invisible `[BB]` / `$[BB]`, and preserving
/// unknown tokens).
pub fn expand_keyword_tokens(
    text: &str,
    locale: &str,
    keywords: &BTreeMap<String, BTreeMap<String, String>>,
) -> String {
    if !text.contains('[') {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let prefix = &rest[..open];
        let after = &rest[open + 1..];
        let Some(close) = after.find(']') else {
            out.push_str(rest);
            return out;
        };
        let key = &after[..close];
        if is_keyword_token(key) {
            let clean_prefix = prefix.strip_suffix('$').unwrap_or(prefix);
            out.push_str(clean_prefix);
            let norm = key.trim().replace(' ', "_");
            if let Some(by_loc) = keywords.get(key).or_else(|| keywords.get(norm.as_str())) {
                if let Some(disp) = by_loc
                    .get(locale)
                    .filter(|s| !s.is_empty())
                    .or_else(|| by_loc.get("en_US").filter(|s| !s.is_empty()))
                {
                    out.push_str(disp);
                } else if norm != "BB" {
                    out.push('[');
                    out.push_str(key);
                    out.push(']');
                }
            } else if norm != "BB" {
                out.push('[');
                out.push_str(key);
                out.push(']');
            }
        } else {
            out.push_str(prefix);
            out.push('[');
            out.push_str(key);
            out.push(']');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Returns true when the entire text is a standalone parenthetical rules reminder (e.g.
/// `"(If I leave the Expedition zone, remove me from the game.)"`).
pub fn is_pure_reminder_text(text: &str) -> bool {
    let t = text.trim();
    let t = t.strip_suffix('.').unwrap_or(t).trim_end();
    if !(t.starts_with('(') && t.ends_with(')')) || t.len() < 3 {
        return false;
    }
    let mut depth: i32 = 0;
    for (idx, ch) in t.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return idx + ch.len_utf8() == t.len();
                }
            }
            _ => {}
        }
    }
    false
}

/// Strip a trailing standalone rules-reminder sentence `(...)` from an output label for
/// `/api/v2/effects`, while keeping integrated qualifiers like `(as an exhausted Mana Orb).` or
/// `({T}).` intact and never emptying a string.
pub fn strip_trailing_reminder(text: &str) -> &str {
    let mut current = text.trim();
    loop {
        let trimmed = current.strip_suffix('.').unwrap_or(current).trim_end();
        if !trimmed.ends_with(')') {
            break;
        }
        let mut depth: i32 = 0;
        let mut open_idx = None;
        for (idx, ch) in trimmed.char_indices().rev() {
            match ch {
                ')' => depth += 1,
                '(' => {
                    depth -= 1;
                    if depth == 0 {
                        open_idx = Some(idx);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(open) = open_idx else {
            break;
        };
        let before = trimmed[..open].trim_end();
        if before.is_empty() {
            break;
        }
        let inside = trimmed[open + 1..trimmed.len() - 1].trim();
        let preceded_by_sentence_end =
            before.ends_with('.') || before.ends_with('!') || before.ends_with('?');
        let inside_ends_sentence = inside.ends_with('.')
            || inside.ends_with('!')
            || inside.ends_with('?')
            || inside.ends_with(".\"")
            || inside.ends_with(".»")
            || inside.ends_with(". »")
            || inside.ends_with(".”");
        if preceded_by_sentence_end || inside_ends_sentence {
            current = before;
        } else {
            break;
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_keyword_normalizes_variants_and_spaces() {
        assert_eq!(canonical_keyword("FLEETING_CHAR").as_deref(), Some("FLEETING"));
        assert_eq!(canonical_keyword("RESUPPLY_INF").as_deref(), Some("RESUPPLY"));
        assert_eq!(canonical_keyword("EXHAUSTED_RESUPPLY").as_deref(), Some("RESUPPLY"));
        assert_eq!(canonical_keyword("COOLDOWN_2").as_deref(), Some("COOLDOWN"));
        assert_eq!(canonical_keyword("AFTER YOU").as_deref(), Some("AFTER_YOU"));
        assert_eq!(canonical_keyword("TOUGH 1").as_deref(), Some("TOUGH"));
        assert_eq!(canonical_keyword("BB"), None);
        assert_eq!(canonical_keyword(""), None);
    }

    #[test]
    fn extract_canonical_keywords_handles_spaces_and_ignores_non_keywords() {
        let mut out = BTreeSet::new();
        extract_canonical_keywords(
            "Gain [AFTER YOU], [TOUGH 1], $[FLEETING_CHAR], $[BB], and [not a kw].",
            &mut out,
        );
        assert_eq!(
            out.into_iter().collect::<Vec<_>>(),
            vec!["AFTER_YOU", "FLEETING", "TOUGH"]
        );
    }

    #[test]
    fn expand_keyword_tokens_strips_dollar_and_bb_and_localizes() {
        let mut kw = BTreeMap::new();
        kw.insert(
            "FLEETING".to_string(),
            BTreeMap::from([
                ("en_US".to_string(), "Fleeting".to_string()),
                ("fr_FR".to_string(), "Fugace".to_string()),
            ]),
        );
        kw.insert(
            "AFTER_YOU".to_string(),
            BTreeMap::from([("fr_FR".to_string(), "Après Vous".to_string())]),
        );
        assert_eq!(
            expand_keyword_tokens("Gagnez $[FLEETING] et [AFTER YOU].$[BB]", "fr_FR", &kw),
            "Gagnez Fugace et Après Vous."
        );
    }

    #[test]
    fn reminder_helpers_distinguish_reminders_from_qualifiers() {
        assert!(is_pure_reminder_text(
            "(If I leave the Expedition zone, remove me from the game.)"
        ));
        assert!(!is_pure_reminder_text("Draw a card."));
        assert_eq!(
            strip_trailing_reminder(
                "Target Character loses [FLEETING]. (Discard me from Reserve to do this.)"
            ),
            "Target Character loses [FLEETING]."
        );
        assert_eq!(
            strip_trailing_reminder(
                "Put target Permanent in its owner's Mana zone (as an exhausted Mana Orb)."
            ),
            "Put target Permanent in its owner's Mana zone (as an exhausted Mana Orb)."
        );
    }
}
