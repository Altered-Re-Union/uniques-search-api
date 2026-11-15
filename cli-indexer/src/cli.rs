use index_core::add_extra_filter;
use index_core::audit_missing;
use index_core::build;
use index_core::decode;
use index_core::idgd_collapse;
use index_core::merge;
use index_core::nonunique;
use index_core::query;
use index_core::extra_catalog::ExtraFilterType;
use crate::bench_query;
use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

fn parse_multi_ids_range(s: &str) -> Result<(usize, usize), String> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        return Err("expected MIN-MAX (e.g. 6-12)".to_string());
    }
    let min: usize = parts[0]
        .trim()
        .parse()
        .map_err(|_| "MIN must be a positive integer".to_string())?;
    let max: usize = parts[1]
        .trim()
        .parse()
        .map_err(|_| "MAX must be a positive integer".to_string())?;
    if min == 0 || max == 0 {
        return Err("MIN and MAX must be >= 1".to_string());
    }
    if min > max {
        return Err("MIN must be <= MAX".to_string());
    }
    Ok((min, max))
}

#[derive(Parser)]
#[command(name = "cli-indexer", about = "Index card JSON by idGd into Roaring bitmaps")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum BuildSource {
    /// Crawl Equinox raw JSON under `<root>/json/<SET>/...` (default, current pipeline).
    Json,
    /// Read a CardsData checkout's CSVs under `<root>/data/csv/...` (unique prints only for now;
    /// see `cli-indexer/plans/15-cardsdata-csv-ingestion.md`).
    Cardsdata,
}

#[derive(Subcommand)]
pub enum Command {
    /// Crawl a dataset and write catalog + idGd bitmaps.
    Build {
        /// Dataset root. Meaning depends on `--source`: `json/<SET>/...` for `json` (default),
        /// or a CardsData checkout (`data/csv/...`) for `cardsdata`.
        #[arg(long)]
        root: PathBuf,
        /// Set code (e.g. COREKS, ALIZE, BISE)
        #[arg(long)]
        set: String,
        /// Output directory (writes `<out>/<SET>/...`)
        #[arg(long)]
        out: PathBuf,
        /// Where to read cards from.
        #[arg(long, value_enum, default_value_t = BuildSource::Json)]
        source: BuildSource,
        /// Stop discovery and indexing after this many files (for testing).
        #[arg(long)]
        limit: Option<usize>,
        /// Print build phase timings (read, parse, process, write). Also enabled by CLI_INDEXER_PROFILE=1.
        /// Has no effect with `--source cardsdata` yet.
        #[arg(long)]
        profile: bool,
        /// Collapse idGd entries that share the same element type and effect text (default: true).
        #[arg(long, default_value_t = true)]
        merge_duplicated_abilities: bool,
    },
    /// Build a standalone non-unique index from a CardsData checkout (faction, rarity, product,
    /// serialization, and the 5 stats shared with uniques — see
    /// `cli-indexer/plans/16-nonunique-index.md`).
    BuildNonunique {
        /// CardsData checkout root containing `data/csv/NonUnique/<SET>/...`
        #[arg(long)]
        root: PathBuf,
        /// One set code, or a comma-separated list to combine into one index (e.g.
        /// `CORE,COREKS,ALIZE`) — writes to `<out>/<SET>/nonunique/` for a single set, or directly
        /// to `<out>/nonunique/` for multiple (see `build_nonunique_index`'s doc comment).
        #[arg(long, value_delimiter = ',')]
        set: Vec<String>,
        /// Output directory. For a single `--set`, writes `<out>/<SET>/...`; for multiple, `out`'s
        /// own folder name becomes the combined index's set name.
        #[arg(long)]
        out: PathBuf,
    },
    /// Decode a global bit index to a card reference.
    Decode {
        #[arg(long)]
        catalog: PathBuf,
        #[arg(long)]
        bit: u32,
    },
    /// Query how many cards contain an idGd, or look up a single card by reference.
    #[command(group(
        clap::ArgGroup::new("query_target")
            .required(true)
            .multiple(false)
            .args(["id_gd", "refid"])
    ))]
    Query {
        #[arg(long)]
        index_dir: PathBuf,
        #[arg(long)]
        set: String,
        /// Comma-separated list of idGd values (e.g. `--id-gd 24,191,76`).
        #[arg(long, value_delimiter = ',', group = "query_target", conflicts_with = "refid")]
        id_gd: Vec<u32>,
        /// Look up a single card by reference (e.g. `ALT_COREKS_B_AX_04_U_10`).
        #[arg(
            long,
            group = "query_target",
            conflicts_with_all = ["id_gd", "list", "show_effect", "whole_card"]
        )]
        refid: Option<String>,
        /// Decode and print up to N matching card references.
        #[arg(long, conflicts_with = "refid")]
        list: Option<usize>,
        /// Show translated effect text instead of a table.
        #[arg(long, default_value_t = false, conflicts_with = "refid")]
        show_effect: bool,
        /// Locale key for effect translation (e.g. en_US, fr_FR).
        #[arg(long, default_value = "en_US")]
        locale: String,
        /// Use whole-card combined bitmaps (`{id}.roar`) instead of per-line sub-indexes.
        #[arg(long, default_value_t = false, conflicts_with = "refid")]
        whole_card: bool,
    },
    /// Merge multiple existing per-SET indexes into one merged index.
    Merge {
        /// Directory containing per-SET folders, e.g. `<index-dir>/<SET>/catalog.json`.
        #[arg(long)]
        index_dir: PathBuf,
        /// Comma-separated list of SET codes in precedence order (used for overlap grouping and tie-breaking).
        #[arg(long)]
        sets: String,
        /// Full output directory for merged index (files written directly under this folder).
        #[arg(long)]
        out: PathBuf,
        /// Collapse idGd entries that share the same element type and effect text (default: true).
        #[arg(long, default_value_t = true)]
        merge_duplicated_abilities: bool,
    },
    /// Find missing cards in gap-suspect families (max_unique_id != card_count).
    AuditMissing {
        /// Directory containing per-SET folders, e.g. `<index-dir>/<SET>/catalog.json`.
        #[arg(long)]
        index_dir: PathBuf,
        #[arg(long)]
        set: String,
        /// Emit JSON keyed by `ALT_<SET>_B_<family_id>` with arrays of missing references.
        #[arg(long, default_value_t = false)]
        json: bool,
    },
    /// Benchmark random idGd queries against an existing index (preloads bitmaps + cards.bin).
    BenchQuery {
        #[arg(long)]
        index_dir: PathBuf,
        #[arg(long)]
        set: String,
        /// Number of timed queries to run.
        #[arg(long, default_value_t = 5000)]
        queries: usize,
        /// Simulate multi-id queries by picking K ids per query (K is random in MIN-MAX),
        /// then doing (TRIGGER union) ∩ (CONDITION union) ∩ (OUTPUT union) (skipping empty groups).
        /// Example: `--multi-ids 6-12`.
        #[arg(long, value_parser = parse_multi_ids_range)]
        multi_ids: Option<(usize, usize)>,
        /// RNG seed (deterministic). If omitted, a default constant is used.
        #[arg(long)]
        seed: Option<u64>,
        /// Warmup iterations (executed but not recorded).
        #[arg(long, default_value_t = 5)]
        warmup: usize,
        /// Optional machine-readable JSON output path.
        #[arg(long)]
        json_out: Option<PathBuf>,
        /// Print first N sampled queries (sanity check; adds output noise).
        #[arg(long)]
        print_samples: Option<usize>,
        /// Use whole-card combined bitmaps (`{id}.roar`) instead of per-line sub-indexes.
        #[arg(long, default_value_t = false)]
        whole_card: bool,
        /// Skip card-decode list ops; still times intersect, count, and window ops.
        #[arg(long, default_value_t = false)]
        roaring_only: bool,
        /// Include per-query cardinality samples in JSON output.
        #[arg(long, default_value_t = false)]
        json_samples: bool,
    },
    /// Collapse idGd entries with identical effect text on an existing index.
    DedupAbilities {
        /// Index directory (contains manifest.json, idgd_catalog.json, id_gd/, cards.bin)
        #[arg(long)]
        index_dir: PathBuf,
    },
    /// Register a card-list filter built from a refs file on an existing index.
    AddExtraFilter {
        /// Index root containing `catalog.json` and `manifest.json`.
        #[arg(long)]
        index_dir: PathBuf,
        /// Stable filter id (writes `extra/<filter-id>.roar`).
        #[arg(long)]
        filter_id: String,
        /// Text file with one card reference per line.
        #[arg(long)]
        refs_file: PathBuf,
        /// Optional filter category for downstream consumers.
        #[arg(long, value_parser = clap::value_parser!(ExtraFilterType))]
        r#type: Option<ExtraFilterType>,
        /// Store refs as an exception set (AND NOT at query time).
        #[arg(long, default_value_t = false)]
        negated: bool,
        /// Overwrite an existing filter with the same `--filter-id`.
        #[arg(long, default_value_t = false)]
        replace: bool,
    },
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build {
            root,
            set,
            out,
            source,
            limit,
            profile,
            merge_duplicated_abilities,
        } => {
            let build_options = build::BuildOptions {
                file_limit: limit,
                profile,
                merge_duplicated_abilities,
            };
            let summary = match source {
                BuildSource::Json => build::build(&root, &set, &out, build_options)?,
                BuildSource::Cardsdata => {
                    build::build_from_cardsdata(&root, &set, &out, build_options)?
                }
            };
            let limit_note = match summary.file_limit {
                Some(n) if summary.stopped_early => format!(" (limit {n})"),
                Some(n) => format!(" (under limit {n})"),
                None => String::new(),
            };
            println!(
                "built {}: {} files{}, {} families, {} idGd bitmaps, bit span {}",
                summary.output_dir.display(),
                summary.files_processed,
                limit_note,
                summary.catalog.families.len(),
                summary.id_gd_count,
                summary.catalog.total_bit_span
            );
        }
        Command::BuildNonunique { root, set, out } => {
            let summary = nonunique::build_nonunique_index(&root, &set, &out)?;
            println!(
                "built {}: {} non-unique prints",
                summary.output_dir.display(),
                summary.cards_indexed
            );
        }
        Command::Decode { catalog, bit } => {
            let decoded = decode::decode_bit(&catalog, bit)?;
            println!("{}", decoded.reference);
            println!(
                "familyId={} uniqueID={}",
                decoded.family_id, decoded.unique_id
            );
        }
        Command::Query {
            index_dir,
            set,
            id_gd,
            refid,
            list,
            show_effect,
            locale,
            whole_card,
        } => {
            if let Some(refid) = refid {
                let card = query::query_refid_effect_text(&index_dir, &set, &refid, &locale)?;
                println!("query: 1 cards");
                println!();
                print_effect_card(&card);
            } else if show_effect {
                let result = query::query_id_gds_effect_text(
                    &index_dir, &set, &id_gd, list, &locale, whole_card,
                )?;
                println!("query: {} cards", result.cardinality);
                if !result.recap_lines.is_empty() {
                    println!();
                    for line in &result.recap_lines {
                        println!("{line}");
                    }
                }
                if !result.cards.is_empty() {
                    println!();
                    for card in &result.cards {
                        print_effect_card(card);
                    }
                }
            } else {
                let result = query::query_id_gds(&index_dir, &set, &id_gd, list, whole_card)?;
                println!("query: {} cards", result.cardinality);
                if !result.rows.is_empty() {
                    println!();
                    println!(
                        "{:<7}  {:<28}  {:>3} {:>3} {:>2} {:>2} {:>2}  {:<40}  {:<16}",
                        "index", "reference", "Hc", "Rc", "M", "O", "F", "main effect", "echo effect"
                    );
                    println!("{}", "-".repeat(7 + 2 + 28 + 2 + 3 + 1 + 3 + 1 + 2 + 1 + 2 + 1 + 2 + 2 + 40 + 2 + 16));
                    for row in &result.rows {
                        println!(
                            "{:<7}  {:<28}  {:>3} {:>3} {:>2} {:>2} {:>2}  {:<40}  {:<16}",
                            row.card_index,
                            row.reference,
                            row.hand,
                            row.reserve,
                            row.m,
                            row.o,
                            row.f,
                            row.main_effect,
                            if row.echo_effect.is_empty() {
                                "<none>"
                            } else {
                                &row.echo_effect
                            }
                        );
                    }
                }
            }
        }
        Command::Merge {
            index_dir,
            sets,
            out,
            merge_duplicated_abilities,
        } => {
            let summary = merge::merge_indexes(
                &index_dir,
                &sets,
                &out,
                merge::MergeOptions {
                    merge_duplicated_abilities,
                },
            )?;
            println!(
                "merged {}: {} source sets, {} cards, {} families, {} idGd bitmaps, bit span {}",
                summary.output_dir.display(),
                summary.source_sets.len(),
                summary.card_count,
                summary.family_count,
                summary.id_gd_count,
                summary.total_bit_span
            );
        }
        Command::AuditMissing {
            index_dir,
            set,
            json,
        } => {
            audit_missing::run(&index_dir, &set, json)?;
        }
        Command::BenchQuery {
            index_dir,
            set,
            queries,
            multi_ids,
            seed,
            warmup,
            json_out,
            print_samples,
            whole_card,
            roaring_only,
            json_samples,
        } => {
            bench_query::run(
                &index_dir,
                &set,
                bench_query::BenchOptions {
                    queries,
                    seed,
                    warmup,
                    multi_ids,
                    json_out,
                    print_samples,
                    whole_card,
                    roaring_only,
                    json_samples,
                },
            )?;
        }
        Command::DedupAbilities { index_dir } => {
            let summary = idgd_collapse::dedup_abilities_on_disk(&index_dir)?;
            if summary.collapsed_pairs == 0 {
                println!(
                    "dedup-abilities {}: 0 new collapses (id_gd={})",
                    summary.index_dir.display(),
                    summary.id_gd_after
                );
            } else {
                println!(
                    "dedup-abilities {}: {} collapsed, id_gd {} -> {}",
                    summary.index_dir.display(),
                    summary.collapsed_pairs,
                    summary.id_gd_before,
                    summary.id_gd_after
                );
            }
        }
        Command::AddExtraFilter {
            index_dir,
            filter_id,
            refs_file,
            r#type,
            negated,
            replace,
        } => {
            let summary = add_extra_filter::add_extra_filter(&add_extra_filter::AddExtraFilterOptions {
                index_dir,
                filter_id,
                refs_file,
                filter_type: r#type,
                negated,
                replace,
            })?;
            let verb = if summary.replaced { "replaced" } else { "added" };
            print!("{verb} extra filter {}", summary.filter_id);
            if let Some(t) = summary.filter_type {
                let type_str = match t {
                    ExtraFilterType::Format => "format",
                    ExtraFilterType::Property => "property",
                };
                print!(" type={type_str}");
            }
            println!(
                " negated={} refs_read={} card_count={} bitmap_bytes={} path={}",
                summary.negated,
                summary.refs_read,
                summary.card_count,
                summary.bitmap_bytes,
                summary.bitmap_path.display()
            );
        }
    }
    Ok(())
}

fn print_effect_card(card: &query::EffectCard) {
    println!("{}", card.reference);
    println!(
        "Cost: {} / {}          Power: O:{} / M:{} / F:{}",
        card.hand, card.reserve, card.o, card.m, card.f
    );
    for line in &card.effect_lines {
        println!("{line}");
    }
    println!("-----------------");
}
