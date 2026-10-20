use crate::catalog::{Catalog, FamilyEntry, FACTION_ORDER};
use crate::compact::RECORD_SIZE;
use crate::bitmap::EffectLine;
use crate::idgd_catalog::{EffectRegionFlags, IdGdCatalog};
use crate::idgd_collapse::{
    build_merged_duplicated_id_gd, collapse_merged_id_gd_on_disk, write_idgd_catalog,
};
use crate::keyword_catalog::KeywordCatalog;
use anyhow::{anyhow, Context, Result};
use roaring::RoaringBitmap;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug)]
pub struct MergeSummary {
    pub output_dir: PathBuf,
    pub source_sets: Vec<String>,
    pub card_count: u32,
    pub family_count: usize,
    pub id_gd_count: usize,
    pub total_bit_span: u32,
}

#[derive(Debug, Deserialize)]
struct BuildManifest {
    set: String,
    card_count: u32,
    total_bit_span: u32,
}

#[derive(Debug)]
struct SourceIndex {
    set: String,
    dir: PathBuf,
    manifest: BuildManifest,
    catalog: Catalog,
    family_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
struct OverlapGroup {
    sets: Vec<String>,
}

#[derive(Debug, Clone)]
enum Mapping {
    Offset { base: u32 },
    RemapVec { map: Vec<u32> }, // length = source total_bit_span
}

#[derive(Debug, Clone)]
struct MergePlan {
    merged_set_name: String,
    source_order: Vec<String>,
    groups: Vec<OverlapGroup>,
    // Per set mapping for card_index -> merged card_index
    mapping: HashMap<String, Mapping>,
    // For fast `cards.bin` copying: destination base for whole-set blocks
    set_base: HashMap<String, u32>,
    // For overlap groups: block_start per (set, family_id)
    block_start: HashMap<(String, String), u32>,
    // Total merged bit span
    total_bit_span: u32,
    // Merged catalog families
    merged_families: Vec<FamilyEntry>,
}

pub struct MergeOptions {
    pub merge_duplicated_abilities: bool,
}

impl Default for MergeOptions {
    fn default() -> Self {
        Self {
            merge_duplicated_abilities: true,
        }
    }
}

pub fn merge_indexes(
    index_dir: &Path,
    sets: &str,
    out: &Path,
    options: MergeOptions,
) -> Result<MergeSummary> {
    let merged_set_name = out
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("--out must end with a folder name"))?
        .to_string();

    let set_list = parse_sets(sets)?;
    let sources = load_sources(index_dir, &set_list)?;

    fs::create_dir_all(out).with_context(|| format!("create {}", out.display()))?;

    let plan = build_plan(&merged_set_name, &set_list, &sources)?;

    write_catalog(out, &merged_set_name, plan.total_bit_span, &plan.merged_families)?;
    write_cards_bin(out, &plan, &sources)?;

    let (_id_gd_count, mut bitmap_sizes, mut idgd_meta) = merge_id_gd(out, &plan, &sources)?;

    let merge_remap = if options.merge_duplicated_abilities {
        collapse_merged_id_gd_on_disk(out, &mut idgd_meta, &mut bitmap_sizes)?
    } else {
        BTreeMap::new()
    };
    let id_gd_count = bitmap_sizes.len();

    merge_stats(out, &plan, &sources)?;
    merge_factions(out, &plan, &sources)?;
    merge_keywords(out, &sources)?;

    let source_dirs: Vec<PathBuf> = sources.iter().map(|s| s.dir.clone()).collect();
    let duplicated_id_gd = build_merged_duplicated_id_gd(&source_dirs, &merge_remap)?;
    write_idgd_catalog(
        out,
        &merged_set_name,
        &bitmap_sizes,
        &idgd_meta,
        &duplicated_id_gd,
    )?;
    write_manifest(out, &merged_set_name, index_dir, &set_list, &sources, plan.total_bit_span, id_gd_count)?;

    let card_count = sources.iter().map(|s| s.manifest.card_count).sum();
    Ok(MergeSummary {
        output_dir: out.to_path_buf(),
        source_sets: set_list,
        card_count,
        family_count: plan.merged_families.len(),
        id_gd_count,
        total_bit_span: plan.total_bit_span,
    })
}

fn parse_sets(sets: &str) -> Result<Vec<String>> {
    let parts: Vec<String> = sets
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .collect();
    if parts.len() < 2 {
        return Err(anyhow!("--sets must contain at least 2 SET codes"));
    }
    let mut seen = HashSet::new();
    for s in &parts {
        if !seen.insert(s.clone()) {
            return Err(anyhow!("duplicate SET in --sets: {s}"));
        }
    }
    Ok(parts)
}

fn load_sources(index_dir: &Path, sets: &[String]) -> Result<Vec<SourceIndex>> {
    let mut sources = Vec::with_capacity(sets.len());
    for set in sets {
        let dir = index_dir.join(set);
        let manifest_path = dir.join("manifest.json");
        let catalog_path = dir.join("catalog.json");

        let manifest_text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?;
        let manifest: BuildManifest = serde_json::from_str(&manifest_text)
            .with_context(|| format!("parse {}", manifest_path.display()))?;
        if manifest.set != *set {
            return Err(anyhow!(
                "manifest set mismatch for {}: expected {}, got {}",
                manifest_path.display(),
                set,
                manifest.set
            ));
        }

        let catalog = Catalog::load(&catalog_path)
            .with_context(|| format!("load {}", catalog_path.display()))?;
        if catalog.set != *set {
            return Err(anyhow!(
                "catalog set mismatch for {}: expected {}, got {}",
                catalog_path.display(),
                set,
                catalog.set
            ));
        }
        if catalog.total_bit_span != manifest.total_bit_span {
            return Err(anyhow!(
                "total_bit_span mismatch for {set}: catalog {}, manifest {}",
                catalog.total_bit_span,
                manifest.total_bit_span
            ));
        }

        let cards_path = dir.join("cards.bin");
        let expected_len = catalog.total_bit_span as u64 * RECORD_SIZE as u64;
        let actual_len = fs::metadata(&cards_path)
            .with_context(|| format!("stat {}", cards_path.display()))?
            .len();
        if actual_len != expected_len {
            return Err(anyhow!(
                "cards.bin size mismatch for {set}: expected {expected_len} bytes, got {actual_len}"
            ));
        }

        let family_ids = catalog
            .families
            .iter()
            .map(|f| f.family_id.clone())
            .collect::<HashSet<_>>();
        sources.push(SourceIndex {
            set: set.clone(),
            dir,
            manifest,
            catalog,
            family_ids,
        });
    }
    Ok(sources)
}

fn build_plan(merged_set_name: &str, set_order: &[String], sources: &[SourceIndex]) -> Result<MergePlan> {
    let source_by_set: HashMap<&str, &SourceIndex> =
        sources.iter().map(|s| (s.set.as_str(), s)).collect();

    let groups = build_overlap_groups(set_order, &source_by_set)?;

    let mut mapping: HashMap<String, Mapping> = HashMap::new();
    let mut set_base: HashMap<String, u32> = HashMap::new();
    let mut block_start: HashMap<(String, String), u32> = HashMap::new();
    let mut merged_families: Vec<FamilyEntry> = Vec::new();

    let mut next_bit: u32 = 0;

    for group in &groups {
        if group.sets.len() == 1 {
            let set = group.sets[0].clone();
            let src = *source_by_set
                .get(set.as_str())
                .ok_or_else(|| anyhow!("missing source {set}"))?;
            set_base.insert(set.clone(), next_bit);
            mapping.insert(set.clone(), Mapping::Offset { base: next_bit });

            // Emit all families in the source catalog order (already AX..YZ, family_number ascending).
            for f in &src.catalog.families {
                let mut out_f = f.clone();
                out_f.start_bit = next_bit + f.start_bit;
                out_f.source_set = Some(set.clone());
                merged_families.push(out_f);
            }
            next_bit = next_bit
                .checked_add(src.catalog.total_bit_span)
                .ok_or_else(|| anyhow!("total_bit_span overflow"))?;
            continue;
        }

        // Multi-set overlap group: lay out by familyId → set → UniqueID.
        // Collect family entries per set by family_id.
        let mut family_by_set: HashMap<&str, HashMap<&str, &FamilyEntry>> = HashMap::new();
        let mut all_family_ids: HashSet<String> = HashSet::new();
        for set in &group.sets {
            let src = *source_by_set
                .get(set.as_str())
                .ok_or_else(|| anyhow!("missing source {set}"))?;
            let mut map = HashMap::new();
            for f in &src.catalog.families {
                map.insert(f.family_id.as_str(), f);
                all_family_ids.insert(f.family_id.clone());
            }
            family_by_set.insert(set.as_str(), map);
        }

        let mut family_ids = all_family_ids.into_iter().collect::<Vec<_>>();
        family_ids.sort_by(|a, b| cmp_family_id(a, b));

        // Prepare per-set remap vectors.
        for set in &group.sets {
            let src = *source_by_set
                .get(set.as_str())
                .ok_or_else(|| anyhow!("missing source {set}"))?;
            mapping.insert(
                set.clone(),
                Mapping::RemapVec {
                    map: vec![u32::MAX; src.catalog.total_bit_span as usize],
                },
            );
        }

        for family_id in &family_ids {
            for set in &group.sets {
                let Some(entry) = family_by_set
                    .get(set.as_str())
                    .and_then(|m| m.get(family_id.as_str()))
                else {
                    continue;
                };

                let start = next_bit;
                block_start.insert((set.clone(), family_id.clone()), start);

                // Fill remap for this family span.
                let Mapping::RemapVec { map } = mapping
                    .get_mut(set)
                    .expect("remap inserted for overlap group")
                else {
                    unreachable!("overlap group set should have RemapVec mapping");
                };

                let src_start = entry.start_bit;
                for u in 0..entry.max_unique_id {
                    let src_idx = src_start + u;
                    let dst_idx = start + u;
                    map[src_idx as usize] = dst_idx;
                }

                // Emit family row in merged catalog with start_bit at this block start.
                let mut out_f = (*entry).clone();
                out_f.start_bit = start;
                out_f.source_set = Some(set.clone());
                merged_families.push(out_f);

                next_bit = next_bit
                    .checked_add(entry.max_unique_id)
                    .ok_or_else(|| anyhow!("total_bit_span overflow"))?;
            }
        }
    }

    Ok(MergePlan {
        merged_set_name: merged_set_name.to_string(),
        source_order: set_order.to_vec(),
        groups,
        mapping,
        set_base,
        block_start,
        total_bit_span: next_bit,
        merged_families,
    })
}

fn build_overlap_groups(
    set_order: &[String],
    source_by_set: &HashMap<&str, &SourceIndex>,
) -> Result<Vec<OverlapGroup>> {
    let mut groups: Vec<OverlapGroup> = Vec::new();
    let mut current: Vec<String> = Vec::new();
    let mut current_families: HashSet<String> = HashSet::new();

    for set in set_order {
        let src = *source_by_set
            .get(set.as_str())
            .ok_or_else(|| anyhow!("missing source index for set {set}"))?;
        if current.is_empty() {
            current.push(set.clone());
            current_families = src.family_ids.clone();
            continue;
        }

        let overlaps = src.family_ids.iter().any(|id| current_families.contains(id));
        if overlaps {
            current.push(set.clone());
            current_families.extend(src.family_ids.iter().cloned());
        } else {
            groups.push(OverlapGroup { sets: current });
            current = vec![set.clone()];
            current_families = src.family_ids.clone();
        }
    }

    if !current.is_empty() {
        groups.push(OverlapGroup { sets: current });
    }
    Ok(groups)
}

fn cmp_family_id(a: &str, b: &str) -> std::cmp::Ordering {
    let (af, an) = split_family_id(a);
    let (bf, bn) = split_family_id(b);
    let ar = faction_rank(af);
    let br = faction_rank(bf);
    ar.cmp(&br).then_with(|| an.cmp(&bn))
}

fn split_family_id(family_id: &str) -> (&str, u32) {
    let mut parts = family_id.split('_');
    let faction = parts.next().unwrap_or("");
    let num = parts
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);
    (faction, num)
}

fn faction_rank(code: &str) -> usize {
    FACTION_ORDER
        .iter()
        .position(|&x| x == code)
        .unwrap_or(usize::MAX)
}

fn write_catalog(out: &Path, merged_set_name: &str, total_bit_span: u32, families: &[FamilyEntry]) -> Result<()> {
    let catalog = Catalog {
        set: merged_set_name.to_string(),
        faction_order: FACTION_ORDER.iter().map(|s| s.to_string()).collect(),
        families: families.to_vec(),
        total_bit_span,
    };
    catalog.save(&out.join("catalog.json"))?;
    Ok(())
}

fn write_cards_bin(out: &Path, plan: &MergePlan, sources: &[SourceIndex]) -> Result<()> {
    let dest_path = out.join("cards.bin");
    let mut dest = OpenOptions::new()
        .create(true)
        .truncate(true)
        .read(true)
        .write(true)
        .open(&dest_path)
        .with_context(|| format!("open {}", dest_path.display()))?;
    dest.set_len(plan.total_bit_span as u64 * RECORD_SIZE as u64)
        .with_context(|| format!("set_len {}", dest_path.display()))?;

    let src_by_set: HashMap<&str, &SourceIndex> = sources.iter().map(|s| (s.set.as_str(), s)).collect();

    // Copy whole-set blocks via base offset.
    for group in &plan.groups {
        if group.sets.len() == 1 {
            let set = group.sets[0].as_str();
            let base = *plan.set_base.get(set).expect("base for single-set group");
            let src = *src_by_set.get(set).expect("src exists");
            let src_path = src.dir.join("cards.bin");

            copy_range(
                &src_path,
                0,
                &mut dest,
                base as u64 * RECORD_SIZE as u64,
                src.catalog.total_bit_span as u64 * RECORD_SIZE as u64,
            )?;
        }
    }

    // Copy overlap groups per family span (contiguous on both sides).
    for group in &plan.groups {
        if group.sets.len() <= 1 {
            continue;
        }
        for set in &group.sets {
            let src = *src_by_set.get(set.as_str()).expect("src exists");
            let src_path = src.dir.join("cards.bin");
            for fam in &src.catalog.families {
                let Some(&dst_start) = plan.block_start.get(&(set.clone(), fam.family_id.clone())) else {
                    continue;
                };
                let src_off = fam.start_bit as u64 * RECORD_SIZE as u64;
                let dst_off = dst_start as u64 * RECORD_SIZE as u64;
                let len = fam.max_unique_id as u64 * RECORD_SIZE as u64;
                copy_range(&src_path, src_off, &mut dest, dst_off, len)?;
            }
        }
    }

    Ok(())
}

fn copy_range(src_path: &Path, src_off: u64, dest: &mut File, dest_off: u64, len: u64) -> Result<()> {
    let mut src = File::open(src_path).with_context(|| format!("open {}", src_path.display()))?;
    src.seek(SeekFrom::Start(src_off))
        .with_context(|| format!("seek {} to {}", src_path.display(), src_off))?;
    dest.seek(SeekFrom::Start(dest_off))
        .with_context(|| format!("seek dest to {}", dest_off))?;

    let mut remaining = len;
    let mut buf = vec![0u8; 1024 * 1024];
    while remaining > 0 {
        let take = remaining.min(buf.len() as u64) as usize;
        src.read_exact(&mut buf[..take])
            .with_context(|| format!("read {} bytes from {}", take, src_path.display()))?;
        dest.write_all(&buf[..take])
            .with_context(|| format!("write {} bytes to dest", take))?;
        remaining -= take as u64;
    }
    Ok(())
}

fn merge_id_gd(
    out: &Path,
    plan: &MergePlan,
    sources: &[SourceIndex],
) -> Result<(
    usize,
    BTreeMap<u32, u64>,
    BTreeMap<u32, (String, BTreeMap<String, crate::card::LocaleText>, EffectRegionFlags)>,
)> {
    let out_dir = out.join("id_gd");
    fs::create_dir_all(&out_dir)?;

    let src_by_set: HashMap<&str, &SourceIndex> = sources.iter().map(|s| (s.set.as_str(), s)).collect();

    // Gather all idGd values + metadata candidates in set order.
    let mut all_ids: BTreeSet<u32> = BTreeSet::new();
    let mut meta: BTreeMap<u32, (String, BTreeMap<String, crate::card::LocaleText>, EffectRegionFlags)> =
        BTreeMap::new();
    let mut effect_flags_by_id: BTreeMap<u32, Vec<EffectRegionFlags>> = BTreeMap::new();
    for set in &plan.source_order {
        let src = *src_by_set.get(set.as_str()).expect("src exists");
        let cat_path = src.dir.join("idgd_catalog.json");
        let cat_text = fs::read_to_string(&cat_path).with_context(|| format!("read {}", cat_path.display()))?;
        let cat: IdGdCatalog = serde_json::from_str(&cat_text).with_context(|| format!("parse {}", cat_path.display()))?;
        for e in cat.entries {
            all_ids.insert(e.id_gd);
            let flags = EffectRegionFlags {
                is_main: e.is_main,
                is_echo: e.is_echo,
            };
            effect_flags_by_id.entry(e.id_gd).or_default().push(flags);
            meta.entry(e.id_gd).or_insert_with(|| {
                (
                    e.element_type,
                    e.translations,
                    flags,
                )
            });
        }
    }
    for (&id_gd, flags) in &effect_flags_by_id {
        if let Some(entry) = meta.get_mut(&id_gd) {
            entry.2 = EffectRegionFlags::merge(flags);
        }
    }

    let mut sizes: BTreeMap<u32, u64> = BTreeMap::new();

    for id in all_ids {
        let mut merged = RoaringBitmap::new();
        for src in sources {
            let src_path = src.dir.join("id_gd").join(format!("{id}.roar"));
            if !src_path.exists() {
                continue;
            }
            let bmp = load_bitmap(&src_path).with_context(|| format!("load {}", src_path.display()))?;
            match plan.mapping.get(&src.set).expect("mapping exists") {
                Mapping::Offset { base } => {
                    for b in bmp.iter() {
                        merged.insert(base + b);
                    }
                }
                Mapping::RemapVec { map } => {
                    for b in bmp.iter() {
                        let dst = map[b as usize];
                        if dst != u32::MAX {
                            merged.insert(dst);
                        }
                    }
                }
            }
        }

        if merged.is_empty() {
            continue;
        }

        let out_path = out_dir.join(format!("{id}.roar"));
        let mut bytes = Vec::new();
        merged.serialize_into(&mut bytes)?;
        let len = bytes.len() as u64;
        fs::write(&out_path, bytes)?;
        sizes.insert(id, len);
    }

    // Merge per-effect-line idGd bitmaps (if present in any source).
    for id in sizes.keys().copied().collect::<Vec<_>>() {
        for line in [EffectLine::M1, EffectLine::M2, EffectLine::M3, EffectLine::Ec] {
            let mut merged = RoaringBitmap::new();
            for src in sources {
                let src_path = src
                    .dir
                    .join("id_gd")
                    .join(format!("{id}_{}.roar", line.suffix()));
                if !src_path.exists() {
                    continue;
                }
                let bmp = load_bitmap(&src_path)
                    .with_context(|| format!("load {}", src_path.display()))?;
                match plan.mapping.get(&src.set).expect("mapping exists") {
                    Mapping::Offset { base } => {
                        for b in bmp.iter() {
                            merged.insert(base + b);
                        }
                    }
                    Mapping::RemapVec { map } => {
                        for b in bmp.iter() {
                            let dst = map[b as usize];
                            if dst != u32::MAX {
                                merged.insert(dst);
                            }
                        }
                    }
                }
            }

            if merged.is_empty() {
                continue;
            }

            let out_path = out_dir.join(format!("{id}_{}.roar", line.suffix()));
            let mut bytes = Vec::new();
            merged.serialize_into(&mut bytes)?;
            fs::write(&out_path, bytes)?;
        }
    }

    Ok((sizes.len(), sizes, meta))
}

fn load_bitmap(path: &Path) -> Result<RoaringBitmap> {
    let bytes = fs::read(path)?;
    Ok(RoaringBitmap::deserialize_from(&bytes[..])?)
}

#[derive(Debug, Serialize)]
struct StatsSummaryOut {
    version: u32,
    set: String,
    total_cards_indexed: u32,
    fields: Vec<StatFieldSummaryOut>,
}

#[derive(Debug, Serialize)]
struct StatFieldSummaryOut {
    field: String,
    element_reference: String,
    counts: BTreeMap<u8, u64>,
    bitmap_dir: String,
}

fn merge_stats(out: &Path, plan: &MergePlan, sources: &[SourceIndex]) -> Result<()> {
    use crate::stat_index::StatField;
    let out_stats = out.join("stats");
    fs::create_dir_all(&out_stats)?;

    let total_cards_indexed: u32 = sources.iter().map(|s| s.manifest.card_count).sum();

    let mut field_summaries: Vec<StatFieldSummaryOut> = Vec::with_capacity(StatField::ALL.len());
    for field in StatField::ALL {
        let mut counts: BTreeMap<u8, u64> = BTreeMap::new();
        let field_dir = out_stats.join(field.dir_name());
        fs::create_dir_all(&field_dir)?;

        for value in 0u8..=15u8 {
            let mut merged = RoaringBitmap::new();
            for src in sources {
                let src_path = src
                    .dir
                    .join("stats")
                    .join(field.dir_name())
                    .join(format!("{value:02}.roar"));
                if !src_path.exists() {
                    continue;
                }
                let bmp = load_bitmap(&src_path)?;
                match plan.mapping.get(&src.set).expect("mapping exists") {
                    Mapping::Offset { base } => {
                        for b in bmp.iter() {
                            merged.insert(base + b);
                        }
                    }
                    Mapping::RemapVec { map } => {
                        for b in bmp.iter() {
                            let dst = map[b as usize];
                            if dst != u32::MAX {
                                merged.insert(dst);
                            }
                        }
                    }
                }
            }

            if merged.is_empty() {
                continue;
            }

            let out_path = field_dir.join(format!("{value:02}.roar"));
            let mut bytes = Vec::new();
            merged.serialize_into(&mut bytes)?;
            fs::write(out_path, bytes)?;
            counts.insert(value, merged.len());
        }

        field_summaries.push(StatFieldSummaryOut {
            field: field.dir_name().to_string(),
            element_reference: field.element_reference().to_string(),
            counts,
            bitmap_dir: format!("stats/{}", field.dir_name()),
        });
    }

    let summary = StatsSummaryOut {
        version: 1,
        set: plan.merged_set_name.clone(),
        total_cards_indexed,
        fields: field_summaries,
    };
    let text = serde_json::to_string_pretty(&summary)?;
    fs::write(out.join("stats_summary.json"), text)?;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct FactionsSummaryIn {
    unknown_count: u64,
}

#[derive(Debug, Serialize)]
struct FactionsSummaryOut {
    version: u32,
    set: String,
    total_cards_indexed: u32,
    source: String,
    factions: Vec<FactionSummaryOut>,
    unknown_count: u64,
    bitmap_dir: String,
}

#[derive(Debug, Serialize)]
struct FactionSummaryOut {
    reference: String,
    card_count: u64,
    bitmap_file: String,
}

fn merge_factions(out: &Path, plan: &MergePlan, sources: &[SourceIndex]) -> Result<()> {
    use crate::faction_index::Faction;
    let out_dir = out.join("factions");
    fs::create_dir_all(&out_dir)?;

    let total_cards_indexed: u32 = sources.iter().map(|s| s.manifest.card_count).sum();

    let mut unknown_count: u64 = 0;
    for src in sources {
        let sum_path = src.dir.join("factions_summary.json");
        if sum_path.exists() {
            let text = fs::read_to_string(&sum_path)?;
            let sum: FactionsSummaryIn = serde_json::from_str(&text)?;
            unknown_count += sum.unknown_count;
        }
    }

    let mut factions_out = Vec::with_capacity(Faction::ALL.len());
    for faction in Faction::ALL {
        let code = faction.reference();
        let mut merged = RoaringBitmap::new();
        for src in sources {
            let src_path = src.dir.join("factions").join(format!("{code}.roar"));
            if !src_path.exists() {
                continue;
            }
            let bmp = load_bitmap(&src_path)?;
            match plan.mapping.get(&src.set).expect("mapping exists") {
                Mapping::Offset { base } => {
                    for b in bmp.iter() {
                        merged.insert(base + b);
                    }
                }
                Mapping::RemapVec { map } => {
                    for b in bmp.iter() {
                        let dst = map[b as usize];
                        if dst != u32::MAX {
                            merged.insert(dst);
                        }
                    }
                }
            }
        }

        if !merged.is_empty() {
            let out_path = out_dir.join(format!("{code}.roar"));
            let mut bytes = Vec::new();
            merged.serialize_into(&mut bytes)?;
            fs::write(&out_path, bytes)?;
        }

        factions_out.push(FactionSummaryOut {
            reference: code.to_string(),
            card_count: merged.len(),
            bitmap_file: format!("factions/{code}.roar"),
        });
    }

    let summary = FactionsSummaryOut {
        version: 1,
        set: plan.merged_set_name.clone(),
        total_cards_indexed,
        source: "mainFaction.reference".to_string(),
        factions: factions_out,
        unknown_count,
        bitmap_dir: "factions".to_string(),
    };
    let text = serde_json::to_string_pretty(&summary)?;
    fs::write(out.join("factions_summary.json"), text)?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct MergeManifestOut {
    version: u32,
    set: String,
    kind: String,
    built_at_secs: u64,
    card_count: u32,
    id_gd_count: usize,
    total_bit_span: u32,
    family_count: usize,
    merge: MergeManifestMergeOut,
}

#[derive(Debug, Serialize)]
struct MergeManifestMergeOut {
    index_dir: String,
    source_sets: Vec<String>,
    source_manifests: Vec<MergeManifestSourceOut>,
}

#[derive(Debug, Serialize)]
struct MergeManifestSourceOut {
    set: String,
    card_count: u32,
    total_bit_span: u32,
}

/// Union of the source `keywords.json` files (first source in `--sets` order wins on conflicts).
fn merge_keywords(out: &Path, sources: &[SourceIndex]) -> Result<()> {
    let mut merged = KeywordCatalog::default();
    for src in sources {
        merged.merge_from(&KeywordCatalog::load_from_dir(&src.dir)?);
    }
    merged.save_in(out)
}

fn write_manifest(
    out: &Path,
    merged_set_name: &str,
    index_dir: &Path,
    set_order: &[String],
    sources: &[SourceIndex],
    total_bit_span: u32,
    id_gd_count: usize,
) -> Result<()> {
    let built_at_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let card_count: u32 = sources.iter().map(|s| s.manifest.card_count).sum();
    let merged_catalog = Catalog::load(&out.join("catalog.json"))?;
    let family_count: usize = merged_catalog.families.len();

    let src_by_set: HashMap<&str, &SourceIndex> = sources.iter().map(|s| (s.set.as_str(), s)).collect();
    let mut source_manifests = Vec::with_capacity(set_order.len());
    for set in set_order {
        let src = *src_by_set.get(set.as_str()).expect("src exists");
        source_manifests.push(MergeManifestSourceOut {
            set: src.set.clone(),
            card_count: src.manifest.card_count,
            total_bit_span: src.manifest.total_bit_span,
        });
    }

    let manifest = MergeManifestOut {
        version: 1,
        set: merged_set_name.to_string(),
        kind: "merge".to_string(),
        built_at_secs,
        card_count,
        id_gd_count,
        total_bit_span,
        family_count,
        merge: MergeManifestMergeOut {
            index_dir: index_dir.display().to_string(),
            source_sets: set_order.to_vec(),
            source_manifests,
        },
    };
    let text = serde_json::to_string_pretty(&manifest)?;
    fs::write(out.join("manifest.json"), text)?;
    Ok(())
}

