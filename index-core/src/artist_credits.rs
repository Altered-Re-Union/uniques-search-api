//! Searchable artist names of a printed artist credit.
//!
//! A print's `artist` is the credit printed on the card (CardsData `Artists.csv` `NickName` of its
//! `ArtistId`). Some credits are teams ("Edward Chee - Seok Yeong", "Ed Chee, S.Yong, Stephen"):
//! CardsData's `ArtistCreditMembers.csv` (`CreditArtistId,MemberArtistId`) links each of them to
//! the artists it is made of — people, and studios such as "Gamon Studio". Searching by artist
//! means searching by those members, not by the printed string:
//!
//! - a credit with rows in `ArtistCreditMembers.csv` → its members' nicknames;
//! - any other credit → itself.
//!
//! The mapping is global (not per-set) CardsData data, written as [`ARTIST_CREDITS_FILE`] at the
//! root of every index built from CardsData (`build --source cardsdata`, `build-nonunique`, kept
//! by `merge`), next to `families.json`. The HTTP API loads it at startup; an index without the
//! file (older build) falls back to "each credit is its own searchable name", see
//! [`ArtistCredits::default`].

use crate::csv_util::read_all;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

/// File name, at the index root.
pub const ARTIST_CREDITS_FILE: &str = "artist_credits.json";

/// On-disk shape of [`ARTIST_CREDITS_FILE`]:
///
/// ```json
/// { "version": 1,
///   "credits": { "Edward Chee - Seok Yeong": ["Edward Cheekokseang", "Gamon Studio", "Seok Yeong"],
///                "Gamon Studio": ["Gamon Studio"] } }
/// ```
///
/// One entry per `Artists.csv` row (keyed by its trimmed `NickName`), its searchable names sorted
/// case-insensitively.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtistCreditsFile {
    pub version: u32,
    pub credits: BTreeMap<String, Vec<String>>,
}

impl ArtistCreditsFile {
    pub const VERSION: u32 = 1;

    pub fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        fs::write(path, text).with_context(|| format!("write {}", path.display()))
    }

    pub fn load(path: &Path) -> Result<Self> {
        let text = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parse {}", path.display()))
    }

    /// Union of several files (e.g. per-set indexes being merged): names of a credit are merged.
    pub fn merge(files: impl IntoIterator<Item = Self>) -> Self {
        let mut credits: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in files {
            for (credit, names) in file.credits {
                let entry = credits.entry(credit).or_default();
                entry.extend(names);
                *entry = sorted_distinct(std::mem::take(entry));
            }
        }
        Self {
            version: Self::VERSION,
            credits,
        }
    }
}

#[derive(Debug, Deserialize)]
struct ArtistRow {
    #[serde(rename = "Id")]
    id: i64,
    #[serde(rename = "NickName")]
    nick_name: String,
}

#[derive(Debug, Deserialize)]
struct ArtistCreditMemberRow {
    #[serde(rename = "CreditArtistId")]
    credit_artist_id: i64,
    #[serde(rename = "MemberArtistId")]
    member_artist_id: i64,
}

/// Reads `<csv_dir>/Artists.csv` and `<csv_dir>/ArtistCreditMembers.csv` (`csv_dir` is a
/// CardsData checkout's `data/csv`). Either file may be missing (older CardsData): no artists →
/// empty mapping; no members file → every credit maps to itself. Member ids missing from
/// `Artists.csv` are ignored.
pub fn build_artist_credits(csv_dir: &Path) -> Result<ArtistCreditsFile> {
    let artists: Vec<ArtistRow> = read_optional(&csv_dir.join("Artists.csv"))?;
    let members: Vec<ArtistCreditMemberRow> =
        read_optional(&csv_dir.join("ArtistCreditMembers.csv"))?;

    let nickname: HashMap<i64, &str> = artists
        .iter()
        .map(|a| (a.id, a.nick_name.trim()))
        .filter(|(_, name)| !name.is_empty())
        .collect();
    let mut members_of: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for row in &members {
        if let Some(member) = nickname.get(&row.member_artist_id) {
            members_of
                .entry(row.credit_artist_id)
                .or_default()
                .push(member.to_string());
        }
    }

    let mut credits: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for artist in &artists {
        let Some(credit) = nickname.get(&artist.id) else {
            continue;
        };
        let names = members_of
            .get(&artist.id)
            .cloned()
            .unwrap_or_else(|| vec![credit.to_string()]);
        let entry = credits.entry(credit.to_string()).or_default();
        entry.extend(names);
        *entry = sorted_distinct(std::mem::take(entry));
    }
    Ok(ArtistCreditsFile {
        version: ArtistCreditsFile::VERSION,
        credits,
    })
}

/// Builds the mapping from a CardsData checkout root and writes it to `<dir>/artist_credits.json`.
pub fn write_artist_credits(cardsdata_root: &Path, dir: &Path) -> Result<()> {
    build_artist_credits(&cardsdata_root.join("data").join("csv"))?
        .save(&dir.join(ARTIST_CREDITS_FILE))
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Vec<T>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    read_all(path)
}

/// Trimmed, non-empty, deduplicated case-insensitively (first spelling kept), sorted
/// case-insensitively.
fn sorted_distinct(names: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut by_key: BTreeMap<String, String> = BTreeMap::new();
    for name in names {
        let name = name.trim();
        if !name.is_empty() {
            by_key
                .entry(name.to_lowercase())
                .or_insert_with(|| name.to_string());
        }
    }
    by_key.into_values().collect()
}

/// In-memory lookup: printed credit → searchable names. The default (empty) value is the fallback
/// used when no mapping is available: every credit is its own searchable name.
#[derive(Debug, Clone, Default)]
pub struct ArtistCredits {
    /// Lowercased trimmed credit → searchable names (display spelling, lowercased).
    by_credit: HashMap<String, Vec<(String, String)>>,
}

impl ArtistCredits {
    pub fn from_file(file: &ArtistCreditsFile) -> Self {
        let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (credit, names) in &file.credits {
            let credit = credit.trim();
            if credit.is_empty() {
                continue;
            }
            grouped
                .entry(credit.to_lowercase())
                .or_default()
                .extend(names.iter().cloned());
        }
        let by_credit = grouped
            .into_iter()
            .map(|(credit, names)| {
                let names = sorted_distinct(names)
                    .into_iter()
                    .map(|n| {
                        let lower = n.to_lowercase();
                        (n, lower)
                    })
                    .collect();
                (credit, names)
            })
            .collect();
        Self { by_credit }
    }

    /// Number of credits with a known mapping (0 = fallback).
    pub fn len(&self) -> usize {
        self.by_credit.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_credit.is_empty()
    }

    /// Searchable names of a printed credit: its members for a team credit, itself otherwise (or
    /// when the credit isn't in the mapping). Empty for an empty credit.
    pub fn searchable_names(&self, credit: &str) -> Vec<String> {
        let credit = credit.trim();
        if credit.is_empty() {
            return Vec::new();
        }
        match self.by_credit.get(&credit.to_lowercase()) {
            Some(names) if !names.is_empty() => names.iter().map(|(n, _)| n.clone()).collect(),
            _ => vec![credit.to_string()],
        }
    }

    /// Whether a print credited `credit` matches one of `wanted` (already trimmed and
    /// lowercased): a wanted name is one of the credit's searchable names, or the printed credit
    /// itself (backward compatibility with clients sending the whole credit string).
    pub fn credit_matches(&self, credit: &str, wanted: &[String]) -> bool {
        let credit = credit.trim();
        if credit.is_empty() || wanted.is_empty() {
            return false;
        }
        let key = credit.to_lowercase();
        if wanted.iter().any(|w| *w == key) {
            return true;
        }
        self.by_credit
            .get(&key)
            .is_some_and(|names| names.iter().any(|(_, lower)| wanted.contains(lower)))
    }
}

/// `artist[]` values as [`ArtistCredits::credit_matches`] expects them: trimmed, lowercased, no
/// empty values.
pub fn normalize_wanted_artists(artists: &[String]) -> Vec<String> {
    artists
        .iter()
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Real CardsData shapes: team credits (1, 25, 57) whose members include the studio 42 and
    /// artists never credited alone (58, 59); a two-person team (46); solo artists.
    fn fixture() -> (TempDir, ArtistCreditsFile) {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Artists.csv"),
            "Id,NickName\n\
1,Edward Chee - Seok Yeong\n\
2,Edward Cheekokseang\n\
7,Ba Vo\n\
8,Anh Tung\n\
11,Zero Wen\n\
25,\"Ed Chee, S.Yong, Stephen\"\n\
42,Gamon Studio\n\
46,Anh Tung - Ba Vo\n\
57,Edward Cheekokseang / Stephen Y\n\
58,Seok Yeong\n\
59,Stephen Yeoh\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("ArtistCreditMembers.csv"),
            "CreditArtistId,MemberArtistId\n\
1,2\n1,58\n1,42\n\
25,2\n25,58\n25,59\n25,42\n\
46,8\n46,7\n\
57,2\n57,59\n57,42\n\
57,999\n",
        )
        .unwrap();
        let file = build_artist_credits(dir.path()).unwrap();
        (dir, file)
    }

    #[test]
    fn builds_members_of_team_credits_and_self_for_the_others() {
        let (_dir, file) = fixture();
        assert_eq!(file.version, 1);
        assert_eq!(
            file.credits["Edward Chee - Seok Yeong"],
            ["Edward Cheekokseang", "Gamon Studio", "Seok Yeong"]
        );
        assert_eq!(
            file.credits["Ed Chee, S.Yong, Stephen"],
            ["Edward Cheekokseang", "Gamon Studio", "Seok Yeong", "Stephen Yeoh"]
        );
        // Unknown member id 999 ignored.
        assert_eq!(
            file.credits["Edward Cheekokseang / Stephen Y"],
            ["Edward Cheekokseang", "Gamon Studio", "Stephen Yeoh"]
        );
        assert_eq!(file.credits["Anh Tung - Ba Vo"], ["Anh Tung", "Ba Vo"]);
        assert_eq!(file.credits["Gamon Studio"], ["Gamon Studio"]);
        assert_eq!(file.credits["Seok Yeong"], ["Seok Yeong"]);
        assert_eq!(file.credits.len(), 11);
    }

    #[test]
    fn missing_members_file_maps_every_credit_to_itself() {
        let dir = TempDir::new().unwrap();
        fs::write(
            dir.path().join("Artists.csv"),
            "Id,NickName\n1,Edward Chee - Seok Yeong\n2, Zero Wen \n",
        )
        .unwrap();
        let file = build_artist_credits(dir.path()).unwrap();
        assert_eq!(file.credits["Edward Chee - Seok Yeong"], ["Edward Chee - Seok Yeong"]);
        assert_eq!(file.credits["Zero Wen"], ["Zero Wen"]);

        let empty = TempDir::new().unwrap();
        assert!(build_artist_credits(empty.path()).unwrap().credits.is_empty());
    }

    #[test]
    fn matches_members_studios_and_the_printed_credit() {
        let (_dir, file) = fixture();
        let credits = ArtistCredits::from_file(&file);
        let wanted = |names: &[&str]| {
            normalize_wanted_artists(&names.iter().map(|n| n.to_string()).collect::<Vec<_>>())
        };

        let seok = wanted(&[" seok yeong "]);
        assert!(credits.credit_matches("Edward Chee - Seok Yeong", &seok));
        assert!(credits.credit_matches("Ed Chee, S.Yong, Stephen", &seok));
        assert!(credits.credit_matches("Seok Yeong", &seok));
        assert!(!credits.credit_matches("Edward Cheekokseang / Stephen Y", &seok));

        let gamon = wanted(&["Gamon Studio"]);
        for credit in [
            "Gamon Studio",
            "Edward Chee - Seok Yeong",
            "Ed Chee, S.Yong, Stephen",
            "Edward Cheekokseang / Stephen Y",
        ] {
            assert!(credits.credit_matches(credit, &gamon), "{credit}");
        }
        assert!(!credits.credit_matches("Anh Tung - Ba Vo", &gamon));

        // The printed team credit itself still matches (backward compatibility).
        assert!(credits.credit_matches("Anh Tung - Ba Vo", &wanted(&["anh tung - ba vo"])));
        assert!(credits.credit_matches("Anh Tung - Ba Vo", &wanted(&["Ba Vo"])));
        assert!(!credits.credit_matches("", &wanted(&["Ba Vo"])));
        // A credit missing from the mapping is its own searchable name.
        assert!(credits.credit_matches("Someone New", &wanted(&["someone new"])));

        assert_eq!(
            credits.searchable_names(" Edward Chee - Seok Yeong "),
            ["Edward Cheekokseang", "Gamon Studio", "Seok Yeong"]
        );
        assert_eq!(credits.searchable_names("Someone New"), ["Someone New"]);
        assert!(credits.searchable_names(" ").is_empty());
    }

    #[test]
    fn fallback_is_exact_credit_match() {
        let credits = ArtistCredits::default();
        assert!(credits.is_empty());
        let wanted = normalize_wanted_artists(&["edward chee - seok yeong".to_string()]);
        assert!(credits.credit_matches("Edward Chee - Seok Yeong", &wanted));
        let seok = normalize_wanted_artists(&["Seok Yeong".to_string()]);
        assert!(!credits.credit_matches("Edward Chee - Seok Yeong", &seok));
        assert_eq!(
            credits.searchable_names("Edward Chee - Seok Yeong"),
            ["Edward Chee - Seok Yeong"]
        );
    }

    #[test]
    fn save_load_and_merge_round_trip() {
        let (dir, file) = fixture();
        let path = dir.path().join(ARTIST_CREDITS_FILE);
        file.save(&path).unwrap();
        assert_eq!(ArtistCreditsFile::load(&path).unwrap(), file);

        let mut other = ArtistCreditsFile::default();
        other
            .credits
            .insert("Gamon Studio".to_string(), vec!["gamon studio".to_string(), "X".to_string()]);
        let merged = ArtistCreditsFile::merge([file.clone(), other]);
        assert_eq!(merged.version, 1);
        assert_eq!(merged.credits["Gamon Studio"], ["Gamon Studio", "X"]);
        assert_eq!(merged.credits.len(), file.credits.len());
    }
}
