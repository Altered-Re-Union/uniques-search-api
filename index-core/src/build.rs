use crate::bitmap::{BitmapStore, PerLineBitmapStore};
use crate::card::{effects_from_card, id_gds_per_effect_line, CardJson};
use crate::catalog::{Catalog, CatalogBuilder};
use crate::compact::{compact_fields_from_card, write_compact_records, CompactCardFields};
use crate::crawl::{discover_card_files, CardFile, DiscoverOptions};
use crate::faction_index::{FactionIndex, FactionIndexBuilder};
use crate::idgd_catalog::IdGdCatalogBuilder;
use crate::idgd_collapse::apply_build_collapse;
use crate::keyword_catalog::KeywordCatalog;
use crate::profile::{profile_enabled, BuildProfile};
use crate::progress::{BuildProgress, DiscoveryProgress, WriteProgress};
use crate::stat_index::{StatIndex, StatIndexBuilder};
use anyhow::Result;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Serialize)]
pub struct Manifest {
    pub version: u32,
    pub set: String,
    pub root: String,
    pub built_at_secs: u64,
    pub card_count: u32,
    pub id_gd_count: usize,
    pub total_bit_span: u32,
    pub family_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_limit: Option<usize>,
}

pub struct BuildOptions {
    pub file_limit: Option<usize>,
    pub profile: bool,
    pub merge_duplicated_abilities: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            file_limit: None,
            profile: false,
            merge_duplicated_abilities: true,
        }
    }
}

pub fn build(
    dataset_root: &Path,
    set: &str,
    out: &Path,
    options: BuildOptions,
) -> Result<BuildSummary> {
    let limit = options.file_limit;
    let profiling = profile_enabled(options.profile);
    let mut profile: Option<BuildProfile> = profiling.then(BuildProfile::default);

    let discovery = DiscoveryProgress::start();

    let discovered = match profile.as_mut() {
        Some(p) => {
            let (result, ns) = BuildProfile::time(|| {
                discover_card_files(
                    dataset_root,
                    set,
                    DiscoverOptions { max_files: limit },
                    Some(&discovery),
                )
            });
            p.discovery_ns = ns;
            result?
        }
        None => {
            discover_card_files(
                dataset_root,
                set,
                DiscoverOptions { max_files: limit },
                Some(&discovery),
            )?
        }
    };

    let files = discovered.files;
    let total_files = files.len();
    discovery.finish(total_files, limit, discovered.stopped_early);

    let progress = BuildProgress::start(total_files);
    let measure_phases = progress.tracks_phases() || profiling;
    let mut catalog_builder = CatalogBuilder::new(set);
    let mut bitmaps = BitmapStore::new();
    let mut per_line_bitmaps = PerLineBitmapStore::new();
    let mut idgd_catalog_builder = IdGdCatalogBuilder::new();
    let mut compact_cards: Vec<(u32, CompactCardFields)> = Vec::with_capacity(total_files);
    let mut stat_index = StatIndexBuilder::new();
    let mut faction_index = FactionIndexBuilder::new();
    let mut keywords = KeywordCatalog::default();

    for file in &files {
        let phases = index_one_card(
            file,
            &mut catalog_builder,
            &mut bitmaps,
            &mut per_line_bitmaps,
            &mut idgd_catalog_builder,
            &mut compact_cards,
            &mut stat_index,
            &mut faction_index,
            &mut keywords,
            profile.as_mut(),
            measure_phases,
        )?;
        if let Some((read_ns, parse_ns, process_ns)) = phases {
            progress.record_card_phases(read_ns, parse_ns, process_ns);
        }
        progress.inc();
    }

    progress.finish("Indexing complete");

    catalog_builder.finalize_last()?;
    let catalog = catalog_builder.into_catalog()?;

    if options.merge_duplicated_abilities {
        apply_build_collapse(
            &mut bitmaps,
            &mut per_line_bitmaps,
            &mut idgd_catalog_builder,
            &mut compact_cards,
        );
    }

    let write_progress = WriteProgress::start();
    let set_out = out.join(set);

    match profile.as_mut() {
        Some(p) => {
            let (_, ns) = BuildProfile::time(|| {
                write_index_outputs(
                    set,
                    dataset_root,
                    &set_out,
                    limit,
                    &catalog,
                    &bitmaps,
                    &per_line_bitmaps,
                    idgd_catalog_builder,
                    compact_cards,
                    stat_index,
                    faction_index,
                    &keywords,
                )
            });
            p.write_ns = ns;
            p.cards_indexed = catalog.total_cards_indexed();
        }
        None => {
            write_index_outputs(
                set,
                dataset_root,
                &set_out,
                limit,
                &catalog,
                &bitmaps,
                &per_line_bitmaps,
                idgd_catalog_builder,
                compact_cards,
                stat_index,
                faction_index,
                &keywords,
            )?;
        }
    }

    write_progress.finish();

    let id_gd_count = bitmaps.len();

    if let Some(p) = profile {
        p.print_report();
    }

    Ok(BuildSummary {
        catalog,
        output_dir: set_out,
        files_processed: total_files,
        id_gd_count,
        file_limit: limit,
        stopped_early: discovered.stopped_early,
    })
}

/// Per-card phase timings `(read_ns, parse_ns, process_ns)` when `measure_phases` is true.
fn index_one_card(
    file: &CardFile,
    catalog_builder: &mut CatalogBuilder,
    bitmaps: &mut BitmapStore,
    per_line_bitmaps: &mut PerLineBitmapStore,
    idgd_catalog_builder: &mut IdGdCatalogBuilder,
    compact_cards: &mut Vec<(u32, CompactCardFields)>,
    stat_index: &mut StatIndexBuilder,
    faction_index: &mut FactionIndexBuilder,
    keywords: &mut KeywordCatalog,
    mut profile: Option<&mut BuildProfile>,
    measure_phases: bool,
) -> Result<Option<(u64, u64, u64)>> {
    let (card, load_timings) = crate::card::load_card_timed(
        &file.path,
        profile.as_deref_mut(),
        measure_phases,
    )?;
    let card_index = catalog_builder.on_card(&file.parsed, &card)?;
    keywords.record_card(&card);

    let mut process = || {
        apply_card_index(
            card_index,
            &card,
            bitmaps,
            per_line_bitmaps,
            idgd_catalog_builder,
            compact_cards,
            stat_index,
            faction_index,
        );
    };

    let process_ns = if measure_phases || profile.is_some() {
        let ((), ns) = BuildProfile::time(&mut process);
        if let Some(p) = profile.as_deref_mut() {
            p.process_ns += ns;
        }
        ns
    } else {
        process();
        0
    };

    if measure_phases {
        Ok(Some((
            load_timings.read_ns,
            load_timings.parse_ns,
            process_ns,
        )))
    } else {
        Ok(None)
    }
}

fn apply_card_index(
    card_index: u32,
    card: &CardJson,
    bitmaps: &mut BitmapStore,
    per_line_bitmaps: &mut PerLineBitmapStore,
    idgd_catalog_builder: &mut IdGdCatalogBuilder,
    compact_cards: &mut Vec<(u32, CompactCardFields)>,
    stat_index: &mut StatIndexBuilder,
    faction_index: &mut FactionIndexBuilder,
) {
    let occurrences = effects_from_card(card);
    for occ in &occurrences {
        idgd_catalog_builder.record_first(occ);
        bitmaps.insert(occ.id_gd, card_index);
    }
    let compact = compact_fields_from_card(card);

    for (line, id_gd) in id_gds_per_effect_line(card) {
        per_line_bitmaps.insert(id_gd, line, card_index);
        idgd_catalog_builder.record_effect_line(id_gd, line);
    }

    stat_index.insert(card_index, &compact);
    faction_index.insert(card_index, &compact);
    compact_cards.push((card_index, compact));
}

fn write_index_outputs(
    set: &str,
    dataset_root: &Path,
    set_out: &Path,
    limit: Option<usize>,
    catalog: &Catalog,
    bitmaps: &BitmapStore,
    per_line_bitmaps: &PerLineBitmapStore,
    idgd_catalog_builder: IdGdCatalogBuilder,
    compact_cards: Vec<(u32, CompactCardFields)>,
    stat_index: StatIndexBuilder,
    faction_index: FactionIndexBuilder,
    keywords: &KeywordCatalog,
) -> Result<()> {
    let id_gd_dir = set_out.join("id_gd");
    fs_create_dir_all(set_out)?;
    catalog.save(&set_out.join("catalog.json"))?;
    let bitmap_bytes = bitmaps.write_dir(&id_gd_dir)?;
    let per_line_bitmap_bytes = per_line_bitmaps.write_dir(&id_gd_dir)?;
    let idgd_catalog = idgd_catalog_builder.build(
        set,
        bitmaps,
        &bitmap_bytes,
        per_line_bitmaps,
        &per_line_bitmap_bytes,
    );
    IdGdCatalogBuilder::save(&idgd_catalog, &set_out.join("idgd_catalog.json"))?;

    keywords.save_in(set_out)?;

    write_compact_records(&set_out.join("cards.bin"), catalog.total_bit_span, &compact_cards)?;

    let stat_index = stat_index.into_index();
    stat_index.write_dir(&set_out.join("stats"))?;
    let stats_summary = stat_index.build_summary(set, catalog.total_cards_indexed());
    StatIndex::save_summary(&stats_summary, &set_out.join("stats_summary.json"))?;

    let faction_index = faction_index.into_index();
    faction_index.write_dir(&set_out.join("factions"))?;
    let factions_summary = faction_index.build_summary(set, catalog.total_cards_indexed());
    FactionIndex::save_summary(&factions_summary, &set_out.join("factions_summary.json"))?;

    let built_at_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let manifest = Manifest {
        version: 1,
        set: set.to_string(),
        root: dataset_root
            .canonicalize()
            .unwrap_or_else(|_| dataset_root.to_path_buf())
            .display()
            .to_string(),
        built_at_secs,
        card_count: catalog.total_cards_indexed(),
        id_gd_count: bitmaps.len(),
        total_bit_span: catalog.total_bit_span,
        family_count: catalog.families.len(),
        file_limit: limit,
    };
    let manifest_text = serde_json::to_string_pretty(&manifest)?;
    std::fs::write(set_out.join("manifest.json"), manifest_text)?;
    Ok(())
}

pub struct BuildSummary {
    pub catalog: Catalog,
    pub output_dir: PathBuf,
    pub files_processed: usize,
    pub id_gd_count: usize,
    pub file_limit: Option<usize>,
    pub stopped_early: bool,
}

fn fs_create_dir_all(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    Ok(())
}
