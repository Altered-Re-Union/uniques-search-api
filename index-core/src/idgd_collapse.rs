use crate::bitmap::{BitmapStore, EffectLine, PerLineBitmapStore};
use crate::card::{translation_text, LocaleText};
use crate::compact::{remap_id_gd_fields, CompactCardFields, RECORD_SIZE};
use crate::idgd_catalog::{
    BitmapMeta, EffectRegionFlags, IdGdCatalog, IdGdCatalogBuilder, IdGdCatalogEntry,
};
use anyhow::{Context, Result};
use roaring::RoaringBitmap;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const COLLAPSE_LOCALE: &str = "en_US";

#[derive(Debug, Clone)]
pub struct CollapseEntry {
    pub id_gd: u32,
    pub element_type: String,
    pub translations: BTreeMap<String, LocaleText>,
}

#[derive(Debug, Default, Clone)]
pub struct IdGdAliasMap {
    alias_to_canonical: BTreeMap<u32, u32>,
}

impl IdGdAliasMap {
    pub fn from_catalog(catalog: &IdGdCatalog) -> Self {
        let mut alias_to_canonical = BTreeMap::new();
        for entry in &catalog.entries {
            for &dup in &entry.duplicated_id_gd {
                alias_to_canonical.insert(dup, entry.id_gd);
            }
        }
        Self { alias_to_canonical }
    }

    pub fn resolve(&self, id: u32) -> u32 {
        self.alias_to_canonical.get(&id).copied().unwrap_or(id)
    }

    /// Return `[canonical, ...aliases]` for `id`, covering both canonical and collapsed alias IDs
    /// so cross-index lookups (e.g. non-unique bitmaps indexed by pre-collapse `idGd`) never miss.
    pub fn equivalent_ids(&self, id: u32) -> Vec<u32> {
        let canonical = self.resolve(id);
        let mut out = vec![canonical];
        for (&alias, &canon) in &self.alias_to_canonical {
            if canon == canonical && alias != canonical {
                out.push(alias);
            }
        }
        out
    }

    pub fn resolve_ids(&self, ids: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            let resolved = self.resolve(id);
            if !out.contains(&resolved) {
                out.push(resolved);
            }
        }
        out
    }

    pub fn is_empty(&self) -> bool {
        self.alias_to_canonical.is_empty()
    }
}

/// Normalize Unicode whitespace for collapse grouping (comparison only).
fn normalize_collapse_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut prev_was_space = false;
    for ch in text.chars() {
        let is_space = ch.is_whitespace() || matches!(ch, '\u{00a0}' | '\u{202f}' | '\u{2007}');
        if is_space {
            if !prev_was_space {
                out.push(' ');
                prev_was_space = true;
            }
        } else {
            out.push(ch);
            prev_was_space = false;
        }
    }
    out
}

fn text_has_nbsp(text: &str) -> bool {
    text.contains('\u{00a0}')
}

fn choose_canonical_id(ids: &[u32], text_by_id: &BTreeMap<u32, String>) -> u32 {
    let nbsp_ids: Vec<u32> = ids
        .iter()
        .copied()
        .filter(|id| text_has_nbsp(&text_by_id[id]))
        .collect();
    if !nbsp_ids.is_empty() {
        return *nbsp_ids.iter().min().unwrap();
    }
    *ids.iter().min().unwrap()
}

/// Map alias idGd -> canonical idGd for entries sharing element type and text.
pub fn build_collapse_remap(entries: impl IntoIterator<Item = CollapseEntry>) -> BTreeMap<u32, u32> {
    let entries: Vec<CollapseEntry> = entries.into_iter().collect();
    let text_by_id: BTreeMap<u32, String> = entries
        .iter()
        .map(|entry| {
            let text = translation_text(&entry.translations, COLLAPSE_LOCALE);
            (entry.id_gd, text)
        })
        .collect();

    let mut groups: BTreeMap<(String, String), Vec<u32>> = BTreeMap::new();
    for entry in &entries {
        let text = match text_by_id.get(&entry.id_gd) {
            Some(t) if !t.is_empty() => t,
            _ => continue,
        };
        groups
            .entry((entry.element_type.clone(), normalize_collapse_text(text)))
            .or_default()
            .push(entry.id_gd);
    }

    let mut remap = BTreeMap::new();
    for ids in groups.values() {
        if ids.len() < 2 {
            continue;
        }
        let canonical = choose_canonical_id(ids, &text_by_id);
        for &id in ids {
            if id != canonical {
                remap.insert(id, canonical);
            }
        }
    }
    remap
}

/// Invert alias -> canonical into canonical -> sorted duplicate ids.
pub fn duplicated_id_gd_from_remap(remap: &BTreeMap<u32, u32>) -> BTreeMap<u32, Vec<u32>> {
    let mut out: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
    for (&alias, &canonical) in remap {
        if alias != canonical {
            out.entry(canonical).or_default().insert(alias);
        }
    }
    out.into_iter()
        .map(|(canonical, set)| {
            let mut v: Vec<u32> = set.into_iter().collect();
            v.sort_unstable();
            (canonical, v)
        })
        .collect()
}

pub fn apply_build_collapse(
    bitmaps: &mut BitmapStore,
    per_line_bitmaps: &mut PerLineBitmapStore,
    idgd_catalog_builder: &mut IdGdCatalogBuilder,
    compact_cards: &mut [(u32, CompactCardFields)],
) {
    let entries = idgd_catalog_builder.collapse_entries();
    let remap = build_collapse_remap(entries);
    if remap.is_empty() {
        return;
    }

    bitmaps.remap_ids(&remap);
    per_line_bitmaps.remap_ids(&remap);
    idgd_catalog_builder.apply_collapse_remap(&remap);

    for (_, fields) in compact_cards.iter_mut() {
        remap_id_gd_fields(fields, &remap);
    }
}

pub fn remap_cards_bin_file(path: &Path, remap: &BTreeMap<u32, u32>) -> Result<()> {
    if remap.is_empty() {
        return Ok(());
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))?;

    let len = file
        .metadata()
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    if len % RECORD_SIZE as u64 != 0 {
        return Err(anyhow::anyhow!(
            "cards.bin length {} is not a multiple of {}",
            len,
            RECORD_SIZE
        ));
    }

    let record_count = (len / RECORD_SIZE as u64) as usize;
    let mut buf = [0u8; RECORD_SIZE];

    for idx in 0..record_count {
        let offset = (idx as u64) * RECORD_SIZE as u64;
        file.seek(SeekFrom::Start(offset))?;
        file.read_exact(&mut buf)?;

        let mut fields = crate::compact::decode_record(&buf);
        remap_id_gd_fields(&mut fields, remap);
        buf = crate::compact::encode_record(&fields);

        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&buf)?;
    }

    Ok(())
}

fn load_bitmap(path: &Path) -> Result<RoaringBitmap> {
    let bytes = fs::read(path)?;
    Ok(RoaringBitmap::deserialize_from(&bytes[..])?)
}

fn write_bitmap(path: &Path, bitmap: &RoaringBitmap) -> Result<u64> {
    let mut bytes = Vec::new();
    bitmap.serialize_into(&mut bytes)?;
    let len = bytes.len() as u64;
    fs::write(path, bytes)?;
    Ok(len)
}

/// Collapse duplicate-text idGd bitmaps on disk after merge; returns merge-time remap.
pub fn collapse_merged_id_gd_on_disk(
    out: &Path,
    meta: &mut BTreeMap<
        u32,
        (
            String,
            BTreeMap<String, LocaleText>,
            crate::idgd_catalog::EffectRegionFlags,
        ),
    >,
    sizes: &mut BTreeMap<u32, u64>,
) -> Result<BTreeMap<u32, u32>> {
    let entries: Vec<CollapseEntry> = meta
        .iter()
        .map(|(&id_gd, (element_type, translations, _))| CollapseEntry {
            id_gd,
            element_type: element_type.clone(),
            translations: translations.clone(),
        })
        .collect();

    let remap = build_collapse_remap(entries);
    if remap.is_empty() {
        return Ok(remap);
    }

    let id_gd_dir = out.join("id_gd");

    for (&alias, &canonical) in &remap {
        if alias == canonical {
            continue;
        }

        let alias_whole = id_gd_dir.join(format!("{alias}.roar"));
        if alias_whole.exists() {
            let alias_bmp = load_bitmap(&alias_whole)?;
            let canonical_whole = id_gd_dir.join(format!("{canonical}.roar"));
            let mut canonical_bmp = if canonical_whole.exists() {
                load_bitmap(&canonical_whole)?
            } else {
                RoaringBitmap::new()
            };
            canonical_bmp |= alias_bmp;
            let bytes = write_bitmap(&canonical_whole, &canonical_bmp)?;
            sizes.insert(canonical, bytes);
            let _ = fs::remove_file(&alias_whole);
        }
        sizes.remove(&alias);

        for line in EffectLine::ALL {
            let alias_path = id_gd_dir.join(format!("{alias}_{}.roar", line.suffix()));
            if !alias_path.exists() {
                continue;
            }
            let alias_bmp = load_bitmap(&alias_path)?;
            let canonical_path = id_gd_dir.join(format!("{canonical}_{}.roar", line.suffix()));
            let mut canonical_bmp = if canonical_path.exists() {
                load_bitmap(&canonical_path)?
            } else {
                RoaringBitmap::new()
            };
            canonical_bmp |= alias_bmp;
            write_bitmap(&canonical_path, &canonical_bmp)?;
            let _ = fs::remove_file(&alias_path);
        }

        if let Some(alias_meta) = meta.remove(&alias) {
            let entry = meta.entry(canonical).or_insert_with(|| {
                (
                    alias_meta.0.clone(),
                    alias_meta.1.clone(),
                    alias_meta.2,
                )
            });
            entry.2 = crate::idgd_catalog::EffectRegionFlags::merge([&entry.2, &alias_meta.2]);
        }
    }

    remap_cards_bin_file(&out.join("cards.bin"), &remap)?;

    Ok(remap)
}

/// Union `duplicated_id_gd` from source catalogs and apply merge-time collapse remap.
pub fn build_merged_duplicated_id_gd(
    source_dirs: &[PathBuf],
    merge_remap: &BTreeMap<u32, u32>,
) -> Result<BTreeMap<u32, Vec<u32>>> {
    fn resolve(id: u32, remap: &BTreeMap<u32, u32>) -> u32 {
        remap.get(&id).copied().unwrap_or(id)
    }

    let mut by_canonical: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();

    for src_dir in source_dirs {
        let path = src_dir.join("idgd_catalog.json");
        if !path.exists() {
            continue;
        }
        let text = fs::read_to_string(&path)
            .with_context(|| format!("read {}", path.display()))?;
        let catalog: IdGdCatalog = serde_json::from_str(&text)
            .with_context(|| format!("parse {}", path.display()))?;

        for entry in catalog.entries {
            let canon = resolve(entry.id_gd, merge_remap);
            for dup in entry.duplicated_id_gd {
                let resolved_dup = resolve(dup, merge_remap);
                if resolved_dup != canon {
                    by_canonical.entry(canon).or_default().insert(resolved_dup);
                }
            }
            if entry.id_gd != canon {
                by_canonical.entry(canon).or_default().insert(entry.id_gd);
            }
        }
    }

    for (&alias, &canonical) in merge_remap {
        if alias != canonical {
            by_canonical.entry(canonical).or_default().insert(alias);
        }
    }

    for (canonical, set) in &mut by_canonical {
        set.remove(canonical);
    }
    by_canonical.retain(|_, set| !set.is_empty());

    Ok(by_canonical
        .into_iter()
        .map(|(canonical, set)| {
            let mut v: Vec<u32> = set.into_iter().collect();
            v.sort_unstable();
            (canonical, v)
        })
        .collect())
}

/// Rewrite `idgd_catalog.json` from on-disk bitmaps and in-memory metadata.
pub fn write_idgd_catalog(
    index_dir: &Path,
    set_name: &str,
    bitmap_sizes: &BTreeMap<u32, u64>,
    meta: &BTreeMap<
        u32,
        (
            String,
            BTreeMap<String, LocaleText>,
            EffectRegionFlags,
        ),
    >,
    duplicated_id_gd: &BTreeMap<u32, Vec<u32>>,
) -> Result<()> {
    let id_gd_dir = index_dir.join("id_gd");
    let mut entries: Vec<IdGdCatalogEntry> = Vec::with_capacity(bitmap_sizes.len());
    for (&id_gd, &bitmap_bytes) in bitmap_sizes {
        let (element_type, translations, flags) = meta
            .get(&id_gd)
            .cloned()
            .map(|(et, tr, f)| (et, tr, f))
            .unwrap_or_else(|| {
                (
                    "UNKNOWN".to_string(),
                    BTreeMap::new(),
                    EffectRegionFlags::default(),
                )
            });
        let bmp_path = id_gd_dir.join(format!("{id_gd}.roar"));
        let bmp = load_bitmap(&bmp_path)?;

        let line_meta = |line: EffectLine| -> Result<Option<BitmapMeta>> {
            let file = format!("{id_gd}_{}.roar", line.suffix());
            let path = id_gd_dir.join(&file);
            if !path.exists() {
                return Ok(None);
            }
            let bmp = load_bitmap(&path)?;
            if bmp.is_empty() {
                return Ok(None);
            }
            let bytes = fs::metadata(&path)
                .with_context(|| format!("stat {}", path.display()))?
                .len() as u64;
            Ok(Some(BitmapMeta {
                card_count: bmp.len(),
                bitmap_bytes: bytes,
                bitmap_file: file,
            }))
        };

        entries.push(IdGdCatalogEntry {
            id_gd,
            card_count: bmp.len(),
            bitmap_bytes,
            bitmap_file: format!("{id_gd}.roar"),
            element_type,
            translations,
            m1: line_meta(EffectLine::M1)?,
            m2: line_meta(EffectLine::M2)?,
            m3: line_meta(EffectLine::M3)?,
            ec: line_meta(EffectLine::Ec)?,
            is_main: flags.is_main,
            is_echo: flags.is_echo,
            duplicated_id_gd: duplicated_id_gd.get(&id_gd).cloned().unwrap_or_default(),
        });
    }
    let cat = IdGdCatalog {
        set: set_name.to_string(),
        entries,
    };
    IdGdCatalogBuilder::save(&cat, &index_dir.join("idgd_catalog.json"))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DedupAbilitiesSummary {
    pub index_dir: PathBuf,
    pub collapsed_pairs: usize,
    pub id_gd_before: usize,
    pub id_gd_after: usize,
}

fn catalog_to_meta_and_sizes(
    catalog: &IdGdCatalog,
) -> (
    BTreeMap<u32, (String, BTreeMap<String, LocaleText>, EffectRegionFlags)>,
    BTreeMap<u32, u64>,
) {
    let mut meta = BTreeMap::new();
    let mut sizes = BTreeMap::new();
    for entry in &catalog.entries {
        meta.insert(
            entry.id_gd,
            (
                entry.element_type.clone(),
                entry.translations.clone(),
                EffectRegionFlags {
                    is_main: entry.is_main,
                    is_echo: entry.is_echo,
                },
            ),
        );
        sizes.insert(entry.id_gd, entry.bitmap_bytes);
    }
    (meta, sizes)
}

/// Collapse whitespace-normalized duplicate idGd entries on an existing index directory.
pub fn dedup_abilities_on_disk(index_dir: &Path) -> Result<DedupAbilitiesSummary> {
    let catalog_path = index_dir.join("idgd_catalog.json");
    let manifest_path = index_dir.join("manifest.json");

    let catalog_text = fs::read_to_string(&catalog_path)
        .with_context(|| format!("read {}", catalog_path.display()))?;
    let catalog: IdGdCatalog = serde_json::from_str(&catalog_text)
        .with_context(|| format!("parse {}", catalog_path.display()))?;
    let id_gd_before = catalog.entries.len();

    let (mut meta, mut bitmap_sizes) = catalog_to_meta_and_sizes(&catalog);
    let remap = collapse_merged_id_gd_on_disk(index_dir, &mut meta, &mut bitmap_sizes)?;
    let id_gd_after = bitmap_sizes.len();
    let collapsed_pairs = remap.len();

    if !remap.is_empty() {
        let duplicated_id_gd =
            build_merged_duplicated_id_gd(&[index_dir.to_path_buf()], &remap)?;
        write_idgd_catalog(
            index_dir,
            &catalog.set,
            &bitmap_sizes,
            &meta,
            &duplicated_id_gd,
        )?;

        let manifest_text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let mut manifest: serde_json::Value = serde_json::from_str(&manifest_text)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        if let Some(obj) = manifest.as_object_mut() {
            obj.insert("id_gd_count".to_string(), serde_json::json!(id_gd_after));
        }
        fs::write(
            &manifest_path,
            serde_json::to_string_pretty(&manifest)? + "\n",
        )
        .with_context(|| format!("write {}", manifest_path.display()))?;
    }

    Ok(DedupAbilitiesSummary {
        index_dir: index_dir.to_path_buf(),
        collapsed_pairs,
        id_gd_before,
        id_gd_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::LocaleText;
    use crate::idgd_catalog::IdGdCatalogEntry;

    fn locale_text(text: &str) -> BTreeMap<String, LocaleText> {
        BTreeMap::from([(
            "en_US".to_string(),
            LocaleText {
                locale: "en_US".to_string(),
                text: text.to_string(),
            },
        )])
    }

    fn entry(id: u32, element_type: &str, text: &str) -> CollapseEntry {
        CollapseEntry {
            id_gd: id,
            element_type: element_type.to_string(),
            translations: locale_text(text),
        }
    }

    #[test]
    fn build_collapse_remap_groups_same_type_and_text() {
        let remap = build_collapse_remap([
            entry(200, "TRIGGER", "When played"),
            entry(100, "TRIGGER", "When played"),
            entry(300, "TRIGGER", "Other"),
        ]);
        assert_eq!(remap.get(&200), Some(&100));
        assert!(!remap.contains_key(&100));
        assert!(!remap.contains_key(&300));
    }

    #[test]
    fn build_collapse_remap_collapses_nbsp_vs_space() {
        let space_text = "Characters your opponents play can't cost less than {2}.";
        let nbsp_text = "Characters your opponents play can't cost less than\u{00a0}{2}.";
        let remap = build_collapse_remap([
            entry(95, "OUTPUT", space_text),
            entry(214, "OUTPUT", nbsp_text),
        ]);
        assert_eq!(remap.get(&95), Some(&214));
        assert!(!remap.contains_key(&214));
    }

    #[test]
    fn build_collapse_remap_prefers_nbsp_canonical_over_min_id() {
        let space_text = "Characters your opponents play can't cost less than {2}.";
        let nbsp_text = "Characters your opponents play can't cost less than\u{00a0}{2}.";
        let entries = [
            entry(95, "OUTPUT", space_text),
            entry(214, "OUTPUT", nbsp_text),
        ];
        let text_by_id: BTreeMap<u32, String> = entries
            .iter()
            .map(|e| {
                (
                    e.id_gd,
                    translation_text(&e.translations, COLLAPSE_LOCALE),
                )
            })
            .collect();
        let ids = vec![95, 214];
        assert_eq!(choose_canonical_id(&ids, &text_by_id), 214);
        assert!(text_has_nbsp(&text_by_id[&214]));
    }

    #[test]
    fn build_collapse_remap_skips_unrelated_text() {
        let remap = build_collapse_remap([
            entry(95, "OUTPUT", "Alpha"),
            entry(214, "OUTPUT", "Beta"),
        ]);
        assert!(remap.is_empty());
    }

    #[test]
    fn build_collapse_remap_skips_different_element_type() {
        let remap = build_collapse_remap([
            entry(100, "TRIGGER", "Same text"),
            entry(200, "OUTPUT", "Same text"),
        ]);
        assert!(remap.is_empty());
    }

    #[test]
    fn build_collapse_remap_skips_empty_text() {
        let remap = build_collapse_remap([
            entry(100, "TRIGGER", ""),
            entry(200, "TRIGGER", ""),
        ]);
        assert!(remap.is_empty());
    }

    #[test]
    fn duplicated_id_gd_from_remap_inverts() {
        let mut remap = BTreeMap::new();
        remap.insert(200, 100);
        remap.insert(300, 100);
        let dups = duplicated_id_gd_from_remap(&remap);
        assert_eq!(dups.get(&100).map(Vec::as_slice), Some(&[200, 300][..]));
    }

    #[test]
    fn alias_map_resolves_duplicates() {
        let catalog = IdGdCatalog {
            set: "T".to_string(),
            entries: vec![IdGdCatalogEntry {
                id_gd: 100,
                card_count: 1,
                bitmap_bytes: 1,
                bitmap_file: "100.roar".to_string(),
                element_type: "TRIGGER".to_string(),
                translations: locale_text("x"),
                duplicated_id_gd: vec![200, 300],
                m1: None,
                m2: None,
                m3: None,
                ec: None,
                is_main: true,
                is_echo: false,
            }],
        };
        let map = IdGdAliasMap::from_catalog(&catalog);
        assert_eq!(map.resolve(200), 100);
        assert_eq!(map.resolve(100), 100);
        assert_eq!(map.resolve_ids(&[200, 300, 100]), vec![100]);
    }
}
