use index_core::build;
use index_core::catalog::Catalog;
use index_core::keyword_catalog::KeywordCatalog;
use index_core::query;
use std::fs;
use std::path::PathBuf;

fn setup_fixture_dataset() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path().to_path_buf();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../index-core/tests/card-json");

    let cards = [
        ("AX", "06", "ALT_COREKS_B_AX_06_U_5.json"),
        ("MU", "22", "ALT_COREKS_B_MU_22_U_3140.json"),
        ("OR", "16", "ALT_COREKS_B_OR_16_U_6.json"),
    ];

    for (faction, family, file) in cards {
        let dest_dir = root
            .join("json")
            .join("COREKS")
            .join(faction)
            .join(family);
        fs::create_dir_all(&dest_dir).expect("mkdir");
        fs::copy(fixtures.join(file), dest_dir.join(file)).expect("copy");
    }

    (dir, root)
}

#[test]
fn build_query_decode_tmp_fixtures() {
    let (_guard, root) = setup_fixture_dataset();
    let out = tempfile::tempdir().expect("out tempdir");
    let summary = build::build(
        &root,
        "COREKS",
        out.path(),
        build::BuildOptions {
            file_limit: None,
            profile: false,
            merge_duplicated_abilities: false,
        },
    )
    .expect("build");
    assert_eq!(summary.files_processed, 3);
    assert!(summary.id_gd_count > 0);

    let catalog = Catalog::load(&summary.output_dir.join("catalog.json")).expect("catalog");
    assert_eq!(catalog.families.len(), 3);
    assert_eq!(catalog.total_cards_indexed(), 3);

    let ax = catalog
        .families
        .iter()
        .find(|f| f.family_id == "AX_06")
        .expect("AX_06");
    let decoded = catalog.decode_bit(ax.start_bit + 4).expect("decode AX_06 U_5");
    assert_eq!(decoded.reference, "ALT_COREKS_B_AX_06_U_5");

    let result = query::query_id_gd(out.path(), "COREKS", 90, Some(5), false).expect("query");
    assert!(result.cardinality >= 1);

    let card = query::query_refid_effect_text(out.path(), "COREKS", "ALT_COREKS_B_AX_06_U_5", "en_US")
        .expect("query refid");
    assert_eq!(card.reference, "ALT_COREKS_B_AX_06_U_5");
    assert!(!card.effect_lines.is_empty());

    let idgd_text =
        std::fs::read_to_string(summary.output_dir.join("idgd_catalog.json")).expect("idgd catalog");
    let idgd: serde_json::Value = serde_json::from_str(&idgd_text).expect("parse idgd catalog");
    let entries = idgd["entries"].as_array().expect("entries array");
    assert!(!entries.is_empty());
    let sample = entries
        .iter()
        .find(|e| e["id_gd"].as_u64() == Some(90))
        .expect("idGd 90 entry");
    assert_eq!(sample["card_count"].as_u64(), Some(1));
    assert!(sample["bitmap_bytes"].as_u64().unwrap_or(0) > 0);
    assert!(sample["translations"]["en_US"]["text"].is_string());

    // Per-effect-line bitmaps should exist when non-empty and be reflected in the catalog.
    // Fixture `ALT_COREKS_B_AX_06_U_5.json` contains idGd 24 in MAIN_EFFECT line 1 and 2, so:
    // - 24_m1.roar exists
    // - 24_m2.roar exists
    // - 24_m3.roar is omitted
    let sample_24 = entries
        .iter()
        .find(|e| e["id_gd"].as_u64() == Some(24))
        .expect("idGd 24 entry");
    assert!(summary.output_dir.join("id_gd/24_m1.roar").is_file());
    assert!(summary.output_dir.join("id_gd/24_m2.roar").is_file());
    assert!(!summary.output_dir.join("id_gd/24_m3.roar").exists());
    assert_eq!(sample_24["m1"]["card_count"].as_u64(), Some(1));
    assert_eq!(sample_24["m2"]["card_count"].as_u64(), Some(1));
    assert!(sample_24.get("m3").is_none() || sample_24["m3"].is_null());

    // Keyword printed names come from `cardEffectElementDisplays[].cardKeyword` of the fixtures.
    let keywords = KeywordCatalog::load_from_dir(&summary.output_dir).expect("keywords.json");
    assert_eq!(keywords.name("FLEETING", "fr_FR"), Some("Fugace"));
    assert_eq!(keywords.name("ETERNAL", "en_US"), Some("Eternal"));
    assert!(!keywords.keywords.contains_key(""), "`[]` is not a keyword");

    let stats_summary_path = summary.output_dir.join("stats_summary.json");
    assert!(stats_summary_path.is_file(), "stats_summary.json missing");
    let stats_text = fs::read_to_string(&stats_summary_path).expect("stats summary");
    let stats: serde_json::Value = serde_json::from_str(&stats_text).expect("parse stats summary");
    assert_eq!(stats["version"].as_u64(), Some(1));
    assert_eq!(stats["total_cards_indexed"].as_u64(), Some(3));

    let main_cost = stats["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .find(|f| f["field"].as_str() == Some("main_cost"))
        .expect("main_cost field");
    let counts = main_cost["counts"].as_object().expect("counts object");
    assert_eq!(counts["2"].as_u64(), Some(1)); // AX_06 U_5
    assert_eq!(counts["3"].as_u64(), Some(1)); // OR_16 U_6
    assert_eq!(counts["7"].as_u64(), Some(1)); // MU_22 U_3140
    assert!(!counts.contains_key("0"));

    assert!(
        summary
            .output_dir
            .join("stats/main_cost/02.roar")
            .is_file()
    );
    assert!(
        summary
            .output_dir
            .join("stats/main_cost/07.roar")
            .is_file()
    );

    let factions_summary_path = summary.output_dir.join("factions_summary.json");
    assert!(factions_summary_path.is_file(), "factions_summary.json missing");
    let factions_text = fs::read_to_string(&factions_summary_path).expect("factions summary");
    let factions: serde_json::Value =
        serde_json::from_str(&factions_text).expect("parse factions summary");
    assert_eq!(factions["source"].as_str(), Some("mainFaction.reference"));
    assert_eq!(factions["unknown_count"].as_u64(), Some(0));

    let faction_entries = factions["factions"].as_array().expect("factions");
    let yz = faction_entries
        .iter()
        .find(|f| f["reference"].as_str() == Some("YZ"))
        .expect("YZ faction");
    assert_eq!(yz["card_count"].as_u64(), Some(1));
    let or = faction_entries
        .iter()
        .find(|f| f["reference"].as_str() == Some("OR"))
        .expect("OR faction entry");
    assert_eq!(or["card_count"].as_u64(), Some(0));

    assert!(
        summary
            .output_dir
            .join("factions/YZ.roar")
            .is_file(),
        "OR path card must index as YZ from mainFaction"
    );
    assert!(
        !summary.output_dir.join("factions/OR.roar").exists(),
        "path OR must not drive faction index"
    );
}

#[test]
fn build_profile_flag_prints_report() {
    let (_guard, root) = setup_fixture_dataset();
    let out = tempfile::tempdir().expect("out tempdir");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_cli-indexer"))
        .args([
            "build",
            "--root",
            root.to_str().expect("root"),
            "--set",
            "COREKS",
            "--out",
            out.path().to_str().expect("out"),
            "--profile",
        ])
        .output()
        .expect("run build --profile");

    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("build profile (3 cards):"));
    assert!(stdout.contains("read"));
    assert!(stdout.contains("parse"));
    assert!(stdout.contains("process"));
    assert!(stdout.contains("write"));
}
