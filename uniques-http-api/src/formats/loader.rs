use std::collections::BTreeMap;
use std::path::Path;

use roaring::RoaringBitmap;

use crate::config::FormatsSettings;
use crate::index::UniquesIndex;

use super::build::{load_single_format, load_single_format_from_text};
use super::schema::{FormatsManifestEntry, MANIFEST_FILE};
use super::source::{FormatsSource, HttpFormatsSource};

#[derive(Debug, Clone)]
pub enum FormatLoadStatus {
    Ready {
        negated: bool,
        bitmap: RoaringBitmap,
    },
    Failed,
}

#[derive(Debug, Clone)]
pub struct LoadedFormat {
    pub id: String,
    pub status: FormatLoadStatus,
}

#[derive(Debug, Clone, Default)]
pub struct FormatIndex {
    pub by_id: BTreeMap<String, LoadedFormat>,
    pub manifest_versions: BTreeMap<String, u64>,
}

impl FormatIndex {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn get(&self, id: &str) -> Option<&LoadedFormat> {
        self.by_id.get(id)
    }
}

struct LoadedEntryResult {
    id: String,
    version: u64,
    outcome: Result<(bool, RoaringBitmap), String>,
}

pub fn load_format_index(index: &UniquesIndex, settings: &FormatsSettings) -> FormatIndex {
    load_format_index_from_source(index, &FormatsSource::from_config(&settings.source))
}

fn load_format_index_from_source(index: &UniquesIndex, source: &FormatsSource) -> FormatIndex {
    match source {
        FormatsSource::Disk(disk) => load_format_index_from_root(index, disk.root()),
        FormatsSource::Http(http) => load_format_index_from_http(index, http),
    }
}

pub fn read_manifest_versions(source: &FormatsSource) -> Option<BTreeMap<String, u64>> {
    let entries = match source {
        FormatsSource::Disk(disk) => read_manifest_entries_from_disk(disk.root())?,
        FormatsSource::Http(http) => {
            let client = http_client().ok()?;
            fetch_manifest(&client, http.manifest_url()).ok()?
        }
    };
    Some(entries.into_iter().map(|e| (e.id, e.version)).collect())
}

fn read_manifest_entries_from_disk(root: &Path) -> Option<Vec<FormatsManifestEntry>> {
    let manifest_path = root.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&manifest_path).ok()?;
    serde_json::from_str(&text).ok()
}

fn load_format_index_from_root(index: &UniquesIndex, root: &Path) -> FormatIndex {
    let manifest_path = root.join(MANIFEST_FILE);
    let entries: Vec<FormatsManifestEntry> = match std::fs::read_to_string(&manifest_path) {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!(
                    "formats: failed to parse {}: {e:#}",
                    manifest_path.display()
                );
                return FormatIndex::empty();
            }
        },
        Err(e) => {
            eprintln!(
                "formats: failed to read {}: {e:#}",
                manifest_path.display()
            );
            return FormatIndex::empty();
        }
    };

    let results: Vec<LoadedEntryResult> = std::thread::scope(|s| {
        entries
            .iter()
            .map(|entry| {
                let entry = entry.clone();
                let root = root.to_path_buf();
                s.spawn(move || {
                    let file_path = root.join(&entry.path);
                    let outcome = load_single_format(index, &entry, &file_path)
                        .map_err(|e| e.to_string());
                    LoadedEntryResult {
                        id: entry.id,
                        version: entry.version,
                        outcome,
                    }
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("format load thread panicked"))
            .collect()
    });

    merge_loaded_results(results)
}

const HTTP_TIMEOUT_SECS: u64 = 30;

fn http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|e| format!("build http client: {e}"))
}

fn fetch_text(client: &reqwest::blocking::Client, url: &str) -> Result<String, String> {
    let response = client
        .get(url)
        .send()
        .map_err(|e| format!("fetch {url}: {e}"))?
        .error_for_status()
        .map_err(|e| format!("fetch {url}: {e}"))?;
    response.text().map_err(|e| format!("read body {url}: {e}"))
}

fn fetch_manifest(
    client: &reqwest::blocking::Client,
    url: &str,
) -> Result<Vec<FormatsManifestEntry>, String> {
    let text = fetch_text(client, url)?;
    serde_json::from_str(&text).map_err(|e| format!("parse {url}: {e}"))
}

fn load_format_index_from_http(index: &UniquesIndex, http: &HttpFormatsSource) -> FormatIndex {
    let client = match http_client() {
        Ok(client) => client,
        Err(e) => {
            eprintln!("formats: {e}");
            return FormatIndex::empty();
        }
    };

    let entries = match fetch_manifest(&client, http.manifest_url()) {
        Ok(entries) => entries,
        Err(e) => {
            eprintln!("formats: failed to load manifest from {}: {e}", http.manifest_url());
            return FormatIndex::empty();
        }
    };

    let results: Vec<LoadedEntryResult> = std::thread::scope(|s| {
        entries
            .iter()
            .map(|entry| {
                let entry = entry.clone();
                let client = client.clone();
                let url = http.file_url(&entry.path);
                s.spawn(move || {
                    let outcome = fetch_text(&client, &url).and_then(|text| {
                        load_single_format_from_text(index, &entry, &text, &url)
                            .map_err(|e| e.to_string())
                    });
                    LoadedEntryResult {
                        id: entry.id,
                        version: entry.version,
                        outcome,
                    }
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().expect("format load thread panicked"))
            .collect()
    });

    merge_loaded_results(results)
}

fn merge_loaded_results(results: Vec<LoadedEntryResult>) -> FormatIndex {
    let mut by_id = BTreeMap::new();
    let mut manifest_versions = BTreeMap::new();

    for result in results {
        manifest_versions.insert(result.id.clone(), result.version);

        if by_id.contains_key(&result.id) {
            eprintln!(
                "formats: duplicate manifest id {:?}; marking duplicate as failed",
                result.id
            );
            by_id.insert(
                result.id.clone(),
                LoadedFormat {
                    id: result.id,
                    status: FormatLoadStatus::Failed,
                },
            );
            continue;
        }

        match result.outcome {
            Ok((negated, bitmap)) => {
                by_id.insert(
                    result.id.clone(),
                    LoadedFormat {
                        id: result.id,
                        status: FormatLoadStatus::Ready { negated, bitmap },
                    },
                );
            }
            Err(reason) => {
                eprintln!("formats: failed to load format {:?}: {reason}", result.id);
                by_id.insert(
                    result.id.clone(),
                    LoadedFormat {
                        id: result.id,
                        status: FormatLoadStatus::Failed,
                    },
                );
            }
        }
    }

    FormatIndex {
        by_id,
        manifest_versions,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Arc;

    use axum::body::Bytes;
    use index_core::catalog::{Catalog, FamilyEntry, FamilySet};
    use index_core::idgd_catalog::IdGdCatalog;
    use index_core::idgd_collapse::IdGdAliasMap;

    use crate::config::{FormatsSettings, FormatsSourceConfig};
    use crate::index::loader::{
        build_family_lookup_index, build_family_span_groups, build_name_search_index,
        build_set_bitmaps, FactionsSummary, IndexManifest, StatsSummary, SET_CORE,
    };
    use crate::index::UniquesIndex;

    use super::*;

    fn test_index() -> UniquesIndex {
        let catalog = Catalog {
            set: "TEST".to_string(),
            faction_order: vec![],
            families: vec![FamilyEntry {
                start_bit: 0,
                faction: "AX".to_string(),
                family_number: "01".to_string(),
                family_id: "AX_01".to_string(),
                source_set: Some(SET_CORE.to_string()),
                max_unique_id: 2,
                card_count: 2,
                first_reference: "ALT_CORE_B_AX_01_U_1".to_string(),
                name: Default::default(),
                artist: String::new(),
                card_sub_types: vec![],
                set: FamilySet {
                    reference: "CORE".to_string(),
                    name: String::new(),
                    code: None,
                },
            }],
            total_bit_span: 2,
        };
        let set_bitmaps = build_set_bitmaps(&catalog);
        UniquesIndex {
            index_dir: PathBuf::from("/test"),
            catalog: catalog.clone(),
            manifest: IndexManifest {
                version: 1,
                set: "TEST".to_string(),
                kind: None,
                built_at_secs: 0,
                card_count: 2,
                id_gd_count: 0,
                total_bit_span: 2,
                family_count: 1,
            },
            idgd_catalog: IdGdCatalog {
                set: "TEST".to_string(),
                entries: vec![],
            },
            id_gd_aliases: IdGdAliasMap::default(),
            stats_summary: StatsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 2,
                fields: vec![],
            },
            factions_summary: FactionsSummary {
                version: 1,
                set: "TEST".to_string(),
                total_cards_indexed: 2,
                source: String::new(),
                factions: vec![],
                unknown_count: 0,
                bitmap_dir: String::new(),
            },
            cards: vec![],
            id_gd_whole: BTreeMap::new(),
            id_gd_per_line: BTreeMap::new(),
            stats: BTreeMap::new(),
            factions: BTreeMap::new(),
            set_bitmaps,
            name_search_index: build_name_search_index(&catalog),
            family_lookup_index: build_family_lookup_index(&catalog),
            family_span_groups: build_family_span_groups(&catalog),
            effects_body: Arc::new(Bytes::from_static(b"[]")),
            effects_body_gzip: None,
        }
    }

    fn formats_settings(root: &std::path::Path) -> FormatsSettings {
        FormatsSettings {
            source: FormatsSourceConfig::Disk {
                path: root.display().to_string(),
            },
            reload_interval_secs: 0,
        }
    }

    #[test]
    fn loads_manifest_and_format_files() {
        let dir = std::env::temp_dir().join(format!("formats_load_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            r#"[
  {"id":"std","path":"std.json","version":1},
  {"id":"other","path":"other.json","version":1}
]"#,
        )
        .unwrap();
        fs::write(
            dir.join("std.json"),
            r#"{"id":"std","version":1,"included_refs":["ALT_CORE_B_AX_01_U_1"]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("other.json"),
            r#"{"id":"other","version":1,"excluded_sets":["CORE"]}"#,
        )
        .unwrap();

        let index = test_index();
        let loaded = load_format_index(&index, &formats_settings(&dir));
        assert_eq!(loaded.by_id.len(), 2);
        assert!(matches!(
            loaded.get("std").unwrap().status,
            FormatLoadStatus::Ready { negated: false, .. }
        ));
        assert!(matches!(
            loaded.get("other").unwrap().status,
            FormatLoadStatus::Ready { negated: true, .. }
        ));
    }

    #[test]
    fn version_mismatch_marks_failed() {
        let dir = std::env::temp_dir().join(format!("formats_ver_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            r#"[{"id":"std","path":"std.json","version":2}]"#,
        )
        .unwrap();
        fs::write(
            dir.join("std.json"),
            r#"{"id":"std","version":1,"included_refs":["ALT_CORE_B_AX_01_U_1"]}"#,
        )
        .unwrap();

        let loaded = load_format_index(&test_index(), &formats_settings(&dir));
        assert!(matches!(
            loaded.get("std").unwrap().status,
            FormatLoadStatus::Failed
        ));
    }

    #[test]
    fn bad_ref_marks_failed() {
        let dir = std::env::temp_dir().join(format!("formats_ref_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            r#"[{"id":"std","path":"std.json","version":1}]"#,
        )
        .unwrap();
        fs::write(
            dir.join("std.json"),
            r#"{"id":"std","version":1,"included_refs":["NOT_A_REAL_REF"]}"#,
        )
        .unwrap();

        let loaded = load_format_index(&test_index(), &formats_settings(&dir));
        assert!(matches!(
            loaded.get("std").unwrap().status,
            FormatLoadStatus::Failed
        ));
    }

    #[test]
    fn duplicate_manifest_id_marks_failed() {
        let dir = std::env::temp_dir().join(format!("formats_dup_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("manifest.json"),
            r#"[
  {"id":"std","path":"a.json","version":1},
  {"id":"std","path":"b.json","version":1}
]"#,
        )
        .unwrap();
        fs::write(
            dir.join("a.json"),
            r#"{"id":"std","version":1,"included_refs":["ALT_CORE_B_AX_01_U_1"]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("b.json"),
            r#"{"id":"std","version":1,"included_refs":["ALT_CORE_B_AX_01_U_1"]}"#,
        )
        .unwrap();

        let loaded = load_format_index(&test_index(), &formats_settings(&dir));
        assert!(matches!(
            loaded.get("std").unwrap().status,
            FormatLoadStatus::Failed
        ));
    }

    #[test]
    fn broken_manifest_yields_empty_index() {
        let dir = std::env::temp_dir().join(format!("formats_bad_{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("manifest.json"), "not json").unwrap();

        let loaded = load_format_index(&test_index(), &formats_settings(&dir));
        assert!(loaded.by_id.is_empty());
    }
}
