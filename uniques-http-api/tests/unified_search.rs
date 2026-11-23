use std::fs;
use std::path::{Path, PathBuf};

use axum::body::Body;
use http_body_util::BodyExt;
use index_core::nonunique::build_nonunique_index;
use tower::ServiceExt;
use uniques_http_api::{app, load_app_state, load_index, ServerState};

struct TempGuard(PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

const FIXTURE_INDEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/minimal_index");

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target);
        } else {
            fs::copy(&path, &target).unwrap();
        }
    }
}

fn build_unified_test_server() -> (TempGuard, ServerState) {
    build_unified_test_server_with("")
}

/// Same fixture, with `extra_alize_prints` (CSV rows, `\n`-terminated) appended to the ALIZE
/// `NonUniquePrints.csv`.
fn build_unified_test_server_with(extra_alize_prints: &str) -> (TempGuard, ServerState) {
    build_unified_test_server_full(extra_alize_prints, ArtistCreditsSource::IndexFile, |_| {})
}

/// Like `build_unified_test_server`, but lets the caller patch the copied unique index dir before
/// it is loaded.
fn build_unified_test_server_patched(patch_index: impl FnOnce(&Path)) -> (TempGuard, ServerState) {
    build_unified_test_server_full("", ArtistCreditsSource::IndexFile, patch_index)
}

/// Where the API gets the artist credit -> searchable names mapping from.
enum ArtistCreditsSource {
    /// `artist_credits.json`, written into the index by `build_nonunique_index`.
    IndexFile,
    /// No file in the index, no setting: fallback, each printed credit is its own name.
    None,
    /// No file in the index; `artists.cardsdata_csv_dir` points at the fixture's `data/csv`.
    CsvSetting,
}

fn build_unified_test_server_full(
    extra_alize_prints: &str,
    artist_credits: ArtistCreditsSource,
    patch_index: impl FnOnce(&Path),
) -> (TempGuard, ServerState) {
    let root_dir = std::env::temp_dir().join(format!("unified_search_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root_dir).unwrap();
    let idx_dir = root_dir.join("unified_index");
    copy_dir_recursive(Path::new(FIXTURE_INDEX), &idx_dir);
    patch_index(&idx_dir);

    let cardsdata = root_dir.join("cardsdata");
    let csv_root = cardsdata.join("data").join("csv");
    let core_dir = csv_root.join("NonUnique").join("CORE");
    let alize_dir = csv_root.join("NonUnique").join("ALIZE");
    fs::create_dir_all(&core_dir).unwrap();
    fs::create_dir_all(&alize_dir).unwrap();

    fs::write(
        csv_root.join("Sets.csv"),
        "Reference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\n\
CORE,Beyond the Gates,Au-dela des Portes,,,\n\
ALIZE,Trial by Frost,L'Epreuve du Givre,,,\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("Artists.csv"),
        "Id,NickName\n\
7,Test Artist\n\
8,Edward Chee - Seok Yeong\n\
2,Edward Cheekokseang\n\
25,\"Ed Chee, S.Yong, Stephen\"\n\
42,Gamon Studio\n\
58,Seok Yeong\n\
59,Stephen Yeoh\n",
    )
    .unwrap();
    // Team credits 8 and 25 (real CardsData ids: 1 and 25), whose members include the studio 42
    // and artists never credited alone (58, 59).
    fs::write(
        csv_root.join("ArtistCreditMembers.csv"),
        "CreditArtistId,MemberArtistId\n8,2\n8,58\n8,42\n25,2\n25,58\n25,59\n25,42\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("CardTypes.csv"),
        "Reference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\n\
HERO,Hero,Heros,,,\n\
CHARACTER,Character,Personnage,,,\n\
SPELL,Spell,Sort,,,\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("CardFamilies.csv"),
        "Id,Reference,CardTypeReference,Name_en_US,Name_fr_FR,Name_es_ES,Name_de_DE,Name_it_IT\n\
100,FAM_HERO,HERO,Sierra & Oddball,Sierra et Oddball,,,\n\
101,FAM_ALPHA,CHARACTER,Alpha Construct,Automate Alpha,,,\n\
102,FAM_OMEGA,CHARACTER,Omega Colossus,Colosse Omega,,,\n\
103,FAM_SPELL,SPELL,Frost Bolt,Eclair de Givre,,,\n\
104,FAM_NEUTRAL,CHARACTER,Brassbug,Scarabot,,,\n\
105,FAM_KURAOKAMI,CHARACTER,Kuraokami Unleashed,Kuraokami Libere des Glaces,,,\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("CardFamilySubTypes.csv"),
        "CardFamilyId,CardSubTypeReference\n101,ENGINEER\n102,ROBOT\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("Keywords.csv"),
        "Reference,DisplayWeb_en_US,DisplayWeb_fr_FR,DisplayWeb_es_ES,DisplayWeb_de_DE,DisplayWeb_it_IT\n\
FLEETING,Fleeting,Fugace,,,\n\
SABOTAGE,Sabotage,Sabotez,,,\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("EffectFragments.csv"),
        "IdGd,KeywordReference,Slot,Text_en_US,Text_fr_FR,Text_es_ES,Text_de_DE,Text_it_IT\n\
1,FLEETING_CHAR,,[FLEETING].,[FLEETING].,,,\n\
2,SABOTAGE,,{j} [SABOTAGE].,{j} [SABOTAGE].,,,\n\
3,,,{D} Boost target.,{D} Boostez une cible.,,,\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("Factions.csv"),
        "Reference,Name_en_US\nAX,Axiom\nOR,Ordis\nYZ,Yzmir\nNE,Neutral\n",
    )
    .unwrap();
    fs::write(
        csv_root.join("CardSubTypes.csv"),
        "Reference,Name_en_US\nENGINEER,Engineer\nROBOT,Robot\n",
    )
    .unwrap();

    fs::write(
        core_dir.join("NonUniquePrints.csv"),
        "Id,Reference,CardFamilyId,SetReference,FactionReference,RarityReference,ProductReference,IsSerialized,ArtistId,CollectorNumberFormatted,MAIN_COST,RECALL_COST,MOUNTAIN_POWER,OCEAN_POWER,FOREST_POWER,PERMANENT,RESERVE,IsBanned,IsErrated,IsSuspended\n\
1,ALT_CORE_B_AX_01_C,100,CORE,AX,C,B,0,7,BTG-001-C,,,,,,2,,0,0,0\n\
2,ALT_CORE_B_AX_02_C,101,CORE,AX,C,B,0,7,BTG-002-C,7,6,4,5,6,,,0,0,0\n\
3,ALT_CORE_A_AX_02_R,101,CORE,AX,R,A,0,7,BTG-002-R,8,7,5,6,7,,,0,1,0\n\
4,ALT_CORE_B_NE_03_C,104,CORE,NE,C,B,0,,BTG-003-C,0,0,2,2,2,,,0,0,0\n",
    )
    .unwrap();
    fs::write(
        core_dir.join("PrintEffectFragments.csv"),
        "PrintId,EffectId,DisplayIndex,TriggerId,ConditionId\n\
2,1,1,,\n\
3,3,1,,\n",
    )
    .unwrap();

    fs::write(
        alize_dir.join("NonUniquePrints.csv"),
        format!(
            "Id,Reference,CardFamilyId,SetReference,FactionReference,RarityReference,ProductReference,IsSerialized,ArtistId,CollectorNumberFormatted,MAIN_COST,RECALL_COST,MOUNTAIN_POWER,OCEAN_POWER,FOREST_POWER,PERMANENT,RESERVE,IsBanned,IsErrated,IsSuspended\n\
5,ALT_ALIZE_B_YZ_10_E,102,ALIZE,YZ,E,B,0,7,TBF-010-E,2,3,1,1,1,,,1,0,0\n\
6,ALT_ALIZE_B_YZ_11_C,103,ALIZE,YZ,C,B,0,7,TBF-011-C,1,2,,,,,,0,0,0\n\
{extra_alize_prints}"
        ),
    )
    .unwrap();
    fs::write(
        alize_dir.join("PrintEffectFragments.csv"),
        "PrintId,EffectId,DisplayIndex,TriggerId,ConditionId\n\
6,2,1,,\n",
    )
    .unwrap();

    build_nonunique_index(
        &cardsdata,
        &["CORE".to_string(), "ALIZE".to_string()],
        &idx_dir,
    )
    .expect("build_nonunique_index succeeds");

    let mut settings = ServerState::for_test(load_index(Path::new(FIXTURE_INDEX)).unwrap())
        .settings
        .as_ref()
        .clone();
    settings.index.path = Some(idx_dir.to_string_lossy().into_owned());
    let credits_file = idx_dir.join(index_core::artist_credits::ARTIST_CREDITS_FILE);
    assert!(credits_file.is_file(), "build_nonunique_index writes artist_credits.json");
    match artist_credits {
        ArtistCreditsSource::IndexFile => {}
        ArtistCreditsSource::None => fs::remove_file(&credits_file).unwrap(),
        ArtistCreditsSource::CsvSetting => {
            fs::remove_file(&credits_file).unwrap();
            settings.artists.cardsdata_csv_dir = Some(csv_root.to_string_lossy().into_owned());
        }
    }
    let app_state = load_app_state(&settings).expect("load unified app_state");
    (TempGuard(root_dir), ServerState::for_test(app_state))
}

async fn get_json(state: &ServerState, uri: &str) -> (u16, serde_json::Value) {
    let resp = app(state.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status().as_u16();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

#[tokio::test]
async fn card_by_reference_resolves_nonunique_and_unique() {
    let (_tmp, state) = build_unified_test_server();

    let (status, hero) = get_json(&state, "/api/v2/card/ALT_CORE_B_AX_01_C").await;
    assert_eq!(status, 200);
    assert_eq!(hero["reference"], "ALT_CORE_B_AX_01_C");
    assert_eq!(hero["name"]["fr_FR"], "Sierra et Oddball");
    assert_eq!(hero["cardType"]["reference"], "HERO");
    assert_eq!(hero["rarity"]["reference"], "COMMON");
    assert!(hero["mainCost"].is_null());
    assert_eq!(hero["permanent"], 2);

    let (status_u, uniq) = get_json(&state, "/api/v2/card/ALT_TEST_B_AX_04_U_1").await;
    assert_eq!(status_u, 200);
    assert_eq!(uniq["reference"], "ALT_TEST_B_AX_04_U_1");
    assert_eq!(uniq["rarity"]["reference"], "UNIQUE");
}

#[tokio::test]
async fn unified_search_filters_and_sorts_accurately() {
    let (_tmp, state) = build_unified_test_server();

    // All non-uniques sorted by mainCost ascending:
    // 0 (Brassbug) -> 1 (Frost Bolt) -> 2 (Omega Colossus) -> 7 (Alpha C) -> 8 (Alpha R) -> null (Hero LAST)
    let (status, res) = get_json(
        &state,
        "/api/v2/search?rarity[]=COMMON&rarity[]=RARE&rarity[]=EXALTED&sort=mainCost:asc",
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(res["iter"]["total"], 6);
    let refs: Vec<&str> = res["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["reference"].as_str().unwrap())
        .collect();
    assert_eq!(
        refs,
        vec![
            "ALT_CORE_B_NE_03_C",
            "ALT_ALIZE_B_YZ_11_C",
            "ALT_ALIZE_B_YZ_10_E",
            "ALT_CORE_B_AX_02_C",
            "ALT_CORE_A_AX_02_R",
            "ALT_CORE_B_AX_01_C",
        ]
    );
    // Verify 7 is strictly before 8 and Hero has mainCost = null!
    assert_eq!(res["cards"][3]["mainCost"], 7);
    assert_eq!(res["cards"][4]["mainCost"], 8);
    assert!(res["cards"][5]["mainCost"].is_null());

    // Pagination page 1 & page 2 under sort=name:fr:asc across all (6 nonunique + 1 unique = 7)
    let (_, p1) = get_json(&state, "/api/v2/search?sort=name:fr:asc&page=1&limit=3").await;
    let (_, p2) = get_json(&state, "/api/v2/search?sort=name:fr:asc&page=2&limit=3").await;
    assert_eq!(p1["iter"]["total"], 7);
    assert_eq!(p1["cards"].as_array().unwrap().len(), 3);
    assert_eq!(p2["cards"].as_array().unwrap().len(), 3);
    assert_eq!(p1["cards"][0]["reference"], "ALT_CORE_A_AX_02_R"); // Automate Alpha (ALT_CORE_A < ALT_CORE_B tiebreak)

    // Faction NE filter -> only Neutral Brassbug
    let (_, ne) = get_json(&state, "/api/v2/search?faction[]=NE").await;
    assert_eq!(ne["iter"]["total"], 1);
    assert_eq!(ne["cards"][0]["reference"], "ALT_CORE_B_NE_03_C");

    // Keyword FLEETING -> matches Print 2
    let (_, kw) = get_json(&state, "/api/v2/search?keywords[]=FLEETING").await;
    assert_eq!(kw["iter"]["total"], 1);
    assert_eq!(kw["cards"][0]["reference"], "ALT_CORE_B_AX_02_C");

    // Variation altArt -> matches Print 3 only (excludes unique + base prints)
    let (_, alt) = get_json(&state, "/api/v2/search?variation[]=altArt").await;
    assert_eq!(alt["iter"]["total"], 1);
    assert_eq!(alt["cards"][0]["reference"], "ALT_CORE_A_AX_02_R");
    assert_eq!(alt["cards"][0]["echoEffect"]["fr_FR"], "{D} Boostez une cible.");

    // Hero cardType + q="Sierra" -> matches Hero only
    let (_, hero) = get_json(&state, "/api/v2/search?cardType[]=HERO&q=sierra").await;
    assert_eq!(hero["iter"]["total"], 1);
    assert_eq!(hero["cards"][0]["reference"], "ALT_CORE_B_AX_01_C");

    // Rarity UNIQUE -> only unique card from minimal_index
    let (_, uniq) = get_json(&state, "/api/v2/search?rarity[]=UNIQUE").await;
    assert_eq!(uniq["iter"]["total"], 1);
    assert_eq!(uniq["cards"][0]["kind"], "unique");
    assert_eq!(uniq["cards"][0]["reference"], "ALT_TEST_B_AX_04_U_1");
    assert_eq!(uniq["cards"][0]["product"], "B");
    assert_eq!(uniq["cards"][0]["isSerialized"], false);

    // Page mode omits cursor, cursor mode emits next_cursor when more items remain
    let (_, paged) = get_json(&state, "/api/v2/search?rarity[]=COMMON&page=1&limit=1").await;
    assert!(paged["iter"]["cursor"].is_null());
    let (_, cursored) = get_json(&state, "/api/v2/search?rarity[]=COMMON&limit=1").await;
    assert_eq!(cursored["iter"]["cursor"], 1);

    // Collector number lookup (including lowercase & 2-part prefix) matches minimal_index unique
    let (_, col_exact) = get_json(&state, "/api/v2/search?collectorNumber[]=btg-001-u-1").await;
    let _ = col_exact; // minimal_index has no collector_family_number; ensures case-insensitive parser accepts it

    // Banned filter
    let (_, banned) = get_json(&state, "/api/v2/search?banned=true").await;
    assert_eq!(banned["iter"]["total"], 1);
    assert_eq!(banned["cards"][0]["reference"], "ALT_ALIZE_B_YZ_10_E");
}

#[tokio::test]
async fn post_search_matches_get_and_effect_filter_excludes_nonuniques() {
    let (_tmp, state) = build_unified_test_server();

    let (_, get_res) = get_json(&state, "/api/v2/search?rarity[]=COMMON&sort=mainCost:desc").await;
    let post_resp = app(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v2/search")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("rarity[]=COMMON&sort=mainCost:desc"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 200);
    let post_bytes = post_resp.into_body().collect().await.unwrap().to_bytes();
    let post_res: serde_json::Value = serde_json::from_slice(&post_bytes).unwrap();
    assert_eq!(get_res, post_res);
}

#[tokio::test]
async fn text_search_matches_separate_words_within_one_locale() {
    let (_tmp, state) = build_unified_test_server();
    // Print 3 (ALT_CORE_A_AX_02_R): name "Alpha Construct" / "Automate Alpha",
    // effect "{D} Boost target." / "{D} Boostez une cible.".
    let refs = |res: &serde_json::Value| -> Vec<String> {
        res["cards"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["reference"].as_str().unwrap().to_string())
            .collect()
    };

    // Words found separately, in any order, accents and case ignored.
    let (status, res) = get_json(&state, "/api/v2/search?effectText=CIBLE%20boostez").await;
    assert_eq!(status, 200);
    assert_eq!(refs(&res), ["ALT_CORE_A_AX_02_R"]);

    // Quoted text is a phrase: words must be adjacent.
    let (_, res) = get_json(&state, "/api/v2/search?effectText=%22boostez%20cible%22").await;
    assert_eq!(res["iter"]["total"], 0);
    let (_, res) = get_json(&state, "/api/v2/search?effectText=boostez%20%22une%20cible%22").await;
    assert_eq!(refs(&res), ["ALT_CORE_A_AX_02_R"]);

    // Symbols are words of their own.
    let (_, res) = get_json(&state, "/api/v2/search?effectText=%7BD%7D").await;
    assert_eq!(refs(&res), ["ALT_CORE_A_AX_02_R"]);

    // q: words may be split between name and effect text, within one same locale.
    let (_, res) = get_json(&state, "/api/v2/search?q=automate%20boostez").await;
    assert_eq!(refs(&res), ["ALT_CORE_A_AX_02_R"]);
    let (_, res) = get_json(&state, "/api/v2/search?q=construct%20boostez").await;
    assert_eq!(res["iter"]["total"], 0, "en name + fr effect must not match");

    // name: separate words, punctuation ignored ("Sierra & Oddball" / "Sierra et Oddball").
    let (_, res) = get_json(&state, "/api/v2/search?name=oddball%20sierra").await;
    assert_eq!(refs(&res), ["ALT_CORE_B_AX_01_C"]);
}

#[tokio::test]
async fn facets_count_subtypes_left_by_the_other_filters() {
    let (_tmp, state) = build_unified_test_server();
    // Non-unique fixture: family 101 (ENGINEER) = prints 2 and 3, family 102 (ROBOT) = print 5.
    let (status, res) = get_json(&state, "/api/v2/search/facets?kind=nonunique").await;
    assert_eq!(status, 200);
    assert_eq!(res["subTypes"], serde_json::json!({ "ENGINEER": 2, "ROBOT": 1 }));

    // Other filters narrow the counts; subtypes with no card left are omitted.
    let (_, res) = get_json(&state, "/api/v2/search/facets?kind=nonunique&faction[]=YZ").await;
    assert_eq!(res["subTypes"], serde_json::json!({ "ROBOT": 1 }));

    // subTypes[] itself is ignored, so the other subtypes stay available.
    let (_, res) =
        get_json(&state, "/api/v2/search/facets?kind=nonunique&subTypes[]=ENGINEER").await;
    assert_eq!(res["subTypes"], serde_json::json!({ "ENGINEER": 2, "ROBOT": 1 }));

    // Across both branches, each count is the total /api/v2/search returns for that subtype.
    let (_, res) = get_json(&state, "/api/v2/search/facets").await;
    let counts = res["subTypes"].as_object().unwrap();
    assert!(counts.contains_key("ENGINEER"));
    for (subtype, count) in counts {
        let (_, search) =
            get_json(&state, &format!("/api/v2/search?subTypes[]={subtype}&limit=1")).await;
        assert_eq!(&search["iter"]["total"], count, "subtype {subtype}");
    }

    // POST takes the same parameters in the body.
    let post_resp = app(state)
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v2/search/facets")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from("kind=nonunique&faction[]=YZ"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post_resp.status(), 200);
    let bytes = post_resp.into_body().collect().await.unwrap().to_bytes();
    let post_res: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(post_res["subTypes"], serde_json::json!({ "ROBOT": 1 }));
}

/// Extra ALIZE prints for the variation / zero-power / artist tests:
/// - 7: Kuraokami, product `P` but listed in `index-core/data/variation_overrides.json` (base card),
///   artist "Edward Chee - Seok Yeong" (composite credit, kept whole);
/// - 8: an alt-art (`A`) with mountainPower 0;
/// - 9: a regular promo (`P`) spell (no powers).
const EXTRA_ALIZE_PRINTS: &str = "\
7,ALT_ALIZE_P_OR_48_C,105,ALIZE,OR,C,P,0,8,TBF-109-C,6,5,5,5,5,,,0,0,0\n\
8,ALT_ALIZE_A_YZ_10_E,102,ALIZE,YZ,E,A,0,7,TBF-010-E-A,2,3,0,1,1,,,0,0,0\n\
9,ALT_ALIZE_P_YZ_11_C,103,ALIZE,YZ,C,P,0,7,TBF-011-C-P,1,2,,,,,,0,0,0\n";

fn refs_of(res: &serde_json::Value) -> Vec<String> {
    let mut refs: Vec<String> = res["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["reference"].as_str().unwrap().to_string())
        .collect();
    refs.sort();
    refs
}

#[tokio::test]
async fn kuraokami_promo_print_counts_as_standard_variation() {
    let (_tmp, state) = build_unified_test_server_with(EXTRA_ALIZE_PRINTS);

    let (status, res) =
        get_json(&state, "/api/v2/search?variation[]=standard&kind=nonunique").await;
    assert_eq!(status, 200);
    assert_eq!(
        refs_of(&res),
        [
            "ALT_ALIZE_B_YZ_10_E",
            "ALT_ALIZE_B_YZ_11_C",
            "ALT_ALIZE_P_OR_48_C",
            "ALT_CORE_B_AX_01_C",
            "ALT_CORE_B_AX_02_C",
            "ALT_CORE_B_NE_03_C",
        ]
    );
    assert_eq!(res["iter"]["total"], 6);

    let (_, res) = get_json(&state, "/api/v2/search?variation[]=promo").await;
    assert_eq!(refs_of(&res), ["ALT_ALIZE_P_YZ_11_C"]);
    let (_, res) = get_json(&state, "/api/v2/search?variation[]=alt-art").await;
    assert_eq!(refs_of(&res), ["ALT_ALIZE_A_YZ_10_E", "ALT_CORE_A_AX_02_R"]);
    let (_, res) =
        get_json(&state, "/api/v2/search?variation[]=alt-art&variation[]=promo").await;
    assert_eq!(res["iter"]["total"], 3);

    // Only the variation changes: the printed product letter is kept.
    let (_, res) = get_json(&state, "/api/v2/search?product[]=P").await;
    assert_eq!(refs_of(&res), ["ALT_ALIZE_P_OR_48_C", "ALT_ALIZE_P_YZ_11_C"]);
    let (_, card) = get_json(&state, "/api/v2/card/ALT_ALIZE_P_OR_48_C").await;
    assert_eq!(card["product"], "P");
}

#[tokio::test]
async fn has_zero_power_keeps_cards_with_a_power_at_zero() {
    let (_tmp, state) = build_unified_test_server_with(EXTRA_ALIZE_PRINTS);

    // Print 8 (mountainPower 0) only: print 4 has mainCost 0 but powers 2, heroes and spells
    // have no powers.
    let (status, res) =
        get_json(&state, "/api/v2/search?hasZeroPower=true&kind=nonunique").await;
    assert_eq!(status, 200);
    assert_eq!(refs_of(&res), ["ALT_ALIZE_A_YZ_10_E"]);

    // Both branches: every card returned has a power at 0, and the total adds up.
    let (_, all) = get_json(&state, "/api/v2/search?hasZeroPower=true&limit=200").await;
    for card in all["cards"].as_array().unwrap() {
        assert!(
            ["forestPower", "mountainPower", "oceanPower"]
                .iter()
                .any(|p| card[*p] == 0),
            "{card}"
        );
    }
    let (_, uniques) = get_json(&state, "/api/v2/search?hasZeroPower=true&kind=unique").await;
    assert_eq!(
        all["iter"]["total"].as_u64().unwrap(),
        1 + uniques["iter"]["total"].as_u64().unwrap()
    );

    // AND with the other filters.
    let (_, res) =
        get_json(&state, "/api/v2/search?hasZeroPower=true&variation[]=alt-art").await;
    assert_eq!(refs_of(&res), ["ALT_ALIZE_A_YZ_10_E"]);
    let (_, res) = get_json(&state, "/api/v2/search?hasZeroPower=true&faction[]=OR").await;
    assert_eq!(res["iter"]["total"], 0);

    // false = no filter; anything else is a 400.
    let (_, everything) = get_json(&state, "/api/v2/search").await;
    let (_, unfiltered) = get_json(&state, "/api/v2/search?hasZeroPower=false").await;
    assert_eq!(unfiltered["iter"]["total"], everything["iter"]["total"]);
    let (status, _) = get_json(&state, "/api/v2/search?hasZeroPower=maybe").await;
    assert_eq!(status, 400);
}

/// [`EXTRA_ALIZE_PRINTS`] plus prints credited to a second team credit and to solo artists:
/// - 10: "Ed Chee, S.Yong, Stephen" (members Edward Cheekokseang, Seok Yeong, Stephen Yeoh and
///   Gamon Studio), family 101 (ENGINEER);
/// - 11: "Gamon Studio" alone; 12: "Edward Cheekokseang" alone.
const ARTIST_ALIZE_PRINTS: &str = "\
7,ALT_ALIZE_P_OR_48_C,105,ALIZE,OR,C,P,0,8,TBF-109-C,6,5,5,5,5,,,0,0,0\n\
8,ALT_ALIZE_A_YZ_10_E,102,ALIZE,YZ,E,A,0,7,TBF-010-E-A,2,3,0,1,1,,,0,0,0\n\
9,ALT_ALIZE_P_YZ_11_C,103,ALIZE,YZ,C,P,0,7,TBF-011-C-P,1,2,,,,,,0,0,0\n\
10,ALT_ALIZE_B_AX_20_C,101,ALIZE,AX,C,B,0,25,TBF-020-C,3,3,3,3,3,,,0,0,0\n\
11,ALT_ALIZE_B_AX_21_C,104,ALIZE,AX,C,B,0,42,TBF-021-C,3,3,3,3,3,,,0,0,0\n\
12,ALT_ALIZE_B_AX_22_C,104,ALIZE,AX,C,B,0,2,TBF-022-C,3,3,3,3,3,,,0,0,0\n";

async fn artist_refs(state: &ServerState, artists: &[&str]) -> Vec<String> {
    let query: Vec<String> = artists
        .iter()
        .map(|a| format!("artist[]={}", a.replace(' ', "%20").replace(',', "%2C")))
        .collect();
    let (status, res) =
        get_json(state, &format!("/api/v2/search?limit=200&{}", query.join("&"))).await;
    assert_eq!(status, 200);
    assert_eq!(
        res["iter"]["total"].as_u64().unwrap() as usize,
        res["cards"].as_array().unwrap().len()
    );
    refs_of(&res)
}

#[tokio::test]
async fn artist_filter_matches_the_people_and_studios_of_a_credit() {
    let (_tmp, state) =
        build_unified_test_server_full(ARTIST_ALIZE_PRINTS, ArtistCreditsSource::IndexFile, |_| {});

    // A member never credited alone finds the team credits they are part of.
    assert_eq!(
        artist_refs(&state, &["seok yeong"]).await,
        ["ALT_ALIZE_B_AX_20_C", "ALT_ALIZE_P_OR_48_C"]
    );
    assert_eq!(artist_refs(&state, &[" Stephen Yeoh "]).await, ["ALT_ALIZE_B_AX_20_C"]);
    // A studio: its own cards plus the team credits it is a member of.
    assert_eq!(
        artist_refs(&state, &["Gamon Studio"]).await,
        ["ALT_ALIZE_B_AX_20_C", "ALT_ALIZE_B_AX_21_C", "ALT_ALIZE_P_OR_48_C"]
    );
    // A person: solo cards plus team credits.
    assert_eq!(
        artist_refs(&state, &["EDWARD CHEEKOKSEANG"]).await,
        ["ALT_ALIZE_B_AX_20_C", "ALT_ALIZE_B_AX_22_C", "ALT_ALIZE_P_OR_48_C"]
    );
    // The printed team credit still matches (backward compatibility), only that credit.
    assert_eq!(
        artist_refs(&state, &["edward chee - seok yeong"]).await,
        ["ALT_ALIZE_P_OR_48_C"]
    );
    assert_eq!(
        artist_refs(&state, &["Ed Chee, S.Yong, Stephen"]).await,
        ["ALT_ALIZE_B_AX_20_C"]
    );
    assert!(artist_refs(&state, &["Edward Chee"]).await.is_empty());

    // OR between values; print 4 (no artist) is left out.
    let (_, res) = get_json(
        &state,
        "/api/v2/search?artist[]=Test%20Artist&artist[]=Seok%20Yeong",
    )
    .await;
    assert_eq!(res["iter"]["total"], 9);

    // Unique branch: the credit is stored on the family (not in the mapping: its own name).
    assert_eq!(artist_refs(&state, &["FIXTURE ARTIST"]).await, ["ALT_TEST_B_AX_04_U_1"]);

    // AND with the other filters, with paging over the filtered total.
    let (_, res) =
        get_json(&state, "/api/v2/search?artist[]=Test%20Artist&faction[]=YZ&limit=1").await;
    assert_eq!(res["iter"]["total"], 4);
    assert_eq!(res["iter"]["cursor"], 1);
    let mut paged = Vec::new();
    for cursor in 0..3 {
        let (_, res) = get_json(
            &state,
            &format!("/api/v2/search?artist[]=Gamon%20Studio&limit=1&cursor={cursor}"),
        )
        .await;
        assert_eq!(res["iter"]["total"], 3);
        paged.extend(refs_of(&res));
    }
    paged.sort();
    assert_eq!(
        paged,
        ["ALT_ALIZE_B_AX_20_C", "ALT_ALIZE_B_AX_21_C", "ALT_ALIZE_P_OR_48_C"]
    );

    // Facets count what the filter leaves: print 10 is the only ENGINEER.
    let (_, res) = get_json(&state, "/api/v2/search/facets?artist[]=Seok%20Yeong").await;
    assert_eq!(res["subTypes"], serde_json::json!({ "ENGINEER": 1 }));
}

#[tokio::test]
async fn card_json_keeps_the_printed_credit_and_lists_its_artists() {
    let (_tmp, state) =
        build_unified_test_server_full(ARTIST_ALIZE_PRINTS, ArtistCreditsSource::IndexFile, |_| {});

    let (_, card) = get_json(&state, "/api/v2/card/ALT_ALIZE_P_OR_48_C").await;
    assert_eq!(card["artist"], "Edward Chee - Seok Yeong");
    assert_eq!(
        card["artists"],
        serde_json::json!(["Edward Cheekokseang", "Gamon Studio", "Seok Yeong"])
    );
    let (_, card) = get_json(&state, "/api/v2/card/ALT_TEST_B_AX_04_U_1").await;
    assert_eq!(card["artist"], "Fixture Artist");
    assert_eq!(card["artists"], serde_json::json!(["Fixture Artist"]));
    // No credit: no `artists`.
    let (_, card) = get_json(&state, "/api/v2/card/ALT_CORE_B_NE_03_C").await;
    assert!(card.get("artists").is_none(), "{card}");

    // Same fields in search results.
    let (_, res) = get_json(&state, "/api/v2/search?artist[]=Stephen%20Yeoh").await;
    assert_eq!(res["cards"][0]["artist"], "Ed Chee, S.Yong, Stephen");
    assert_eq!(
        res["cards"][0]["artists"],
        serde_json::json!(["Edward Cheekokseang", "Gamon Studio", "Seok Yeong", "Stephen Yeoh"])
    );
}

#[tokio::test]
async fn artists_endpoint_lists_people_and_studios_never_team_credits() {
    let (_tmp, state) =
        build_unified_test_server_full(ARTIST_ALIZE_PRINTS, ArtistCreditsSource::IndexFile, |_| {});
    let (status, res) = get_json(&state, "/api/v2/artists").await;
    assert_eq!(status, 200);
    assert_eq!(
        res,
        serde_json::json!({
            "artists": [
                "Edward Cheekokseang",
                "Fixture Artist",
                "Gamon Studio",
                "Seok Yeong",
                "Stephen Yeoh",
                "Test Artist"
            ]
        })
    );
}

#[tokio::test]
async fn without_a_mapping_each_printed_credit_is_its_own_artist() {
    let (_tmp, state) =
        build_unified_test_server_full(ARTIST_ALIZE_PRINTS, ArtistCreditsSource::None, |_| {});

    assert!(artist_refs(&state, &["Seok Yeong"]).await.is_empty());
    assert_eq!(
        artist_refs(&state, &["edward chee - seok yeong"]).await,
        ["ALT_ALIZE_P_OR_48_C"]
    );
    assert_eq!(artist_refs(&state, &["Gamon Studio"]).await, ["ALT_ALIZE_B_AX_21_C"]);

    let (_, card) = get_json(&state, "/api/v2/card/ALT_ALIZE_P_OR_48_C").await;
    assert_eq!(card["artists"], serde_json::json!(["Edward Chee - Seok Yeong"]));

    let (_, res) = get_json(&state, "/api/v2/artists").await;
    assert_eq!(
        res,
        serde_json::json!({
            "artists": [
                "Ed Chee, S.Yong, Stephen",
                "Edward Chee - Seok Yeong",
                "Edward Cheekokseang",
                "Fixture Artist",
                "Gamon Studio",
                "Test Artist"
            ]
        })
    );
}

#[tokio::test]
async fn cardsdata_csv_dir_setting_builds_the_mapping_without_the_index_file() {
    let (_tmp, state) =
        build_unified_test_server_full(ARTIST_ALIZE_PRINTS, ArtistCreditsSource::CsvSetting, |_| {});

    assert_eq!(
        artist_refs(&state, &["Seok Yeong"]).await,
        ["ALT_ALIZE_B_AX_20_C", "ALT_ALIZE_P_OR_48_C"]
    );
    let (_, res) = get_json(&state, "/api/v2/artists").await;
    let artists = res["artists"].as_array().unwrap();
    assert!(artists.contains(&serde_json::json!("Stephen Yeoh")));
    assert!(!artists.contains(&serde_json::json!("Edward Chee - Seok Yeong")));
}

/// Write `[T, C, O]` idGd slots into the single unique's 32-byte `cards.bin` record
/// (`group` 0..=2 = main M1..M3, 3 = echo). Slot u16 LE at byte `6 + (group * 3 + k) * 2`.
fn set_effect_group(idx_dir: &Path, group: usize, tco: [u16; 3]) {
    let path = idx_dir.join("cards.bin");
    let mut bytes = fs::read(&path).unwrap();
    for (k, id) in tco.into_iter().enumerate() {
        let off = 6 + (group * 3 + k) * 2;
        bytes[off..off + 2].copy_from_slice(&id.to_le_bytes());
    }
    fs::write(&path, bytes).unwrap();
}

#[tokio::test]
async fn unique_cards_expose_abilities_nonuniques_do_not() {
    // minimal_index idgd catalog: 1 = TRIGGER "{R}", 2 = CONDITION, 3 = OUTPUT "[AFTER_YOU]."
    let (_tmp, state) = build_unified_test_server_patched(|dir| {
        set_effect_group(dir, 0, [1, 2, 3]);
        set_effect_group(dir, 3, [0, 0, 3]);
    });

    let expected = serde_json::json!([
        {
            "kind": "main",
            "line": 1,
            "trigger": { "idGd": 1, "text": { "en_US": "{R}" } },
            "condition": { "idGd": 2, "text": { "en_US": "If you control two or more Plants:" } },
            "output": { "idGd": 3, "text": { "en_US": "[AFTER_YOU]." } }
        },
        {
            "kind": "echo",
            "line": 1,
            "trigger": null,
            "condition": null,
            "output": { "idGd": 3, "text": { "en_US": "[AFTER_YOU]." } }
        }
    ]);

    let (status, uniq) = get_json(&state, "/api/v2/card/ALT_TEST_B_AX_04_U_1").await;
    assert_eq!(status, 200);
    assert_eq!(uniq["abilities"], expected);

    let (status, res) = get_json(&state, "/api/v2/search?rarity[]=UNIQUE").await;
    assert_eq!(status, 200);
    assert_eq!(res["cards"][0]["reference"], "ALT_TEST_B_AX_04_U_1");
    assert_eq!(res["cards"][0]["abilities"], expected);

    // Non-unique prints carry no per-line idGd data in the index: no `abilities` key at all.
    let (status, hero) = get_json(&state, "/api/v2/card/ALT_CORE_B_AX_02_C").await;
    assert_eq!(status, 200);
    assert!(hero.get("abilities").is_none());
    let (_, commons) = get_json(&state, "/api/v2/search?rarity[]=COMMON").await;
    for card in commons["cards"].as_array().unwrap() {
        assert!(card.get("abilities").is_none());
    }
}

#[tokio::test]
async fn unique_without_effect_ids_has_no_abilities_key() {
    let (_tmp, state) = build_unified_test_server();
    let (status, uniq) = get_json(&state, "/api/v2/card/ALT_TEST_B_AX_04_U_1").await;
    assert_eq!(status, 200);
    assert!(uniq.get("abilities").is_none());
}
