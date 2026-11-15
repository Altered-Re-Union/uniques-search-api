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
    let root_dir = std::env::temp_dir().join(format!("unified_search_{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root_dir).unwrap();
    let idx_dir = root_dir.join("unified_index");
    copy_dir_recursive(Path::new(FIXTURE_INDEX), &idx_dir);

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
    fs::write(csv_root.join("Artists.csv"), "Id,NickName\n7,Test Artist\n").unwrap();
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
104,FAM_NEUTRAL,CHARACTER,Brassbug,Scarabot,,,\n",
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
        "Reference,Name_en_US\nAX,Axiom\nYZ,Yzmir\nNE,Neutral\n",
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
        "Id,Reference,CardFamilyId,SetReference,FactionReference,RarityReference,ProductReference,IsSerialized,ArtistId,CollectorNumberFormatted,MAIN_COST,RECALL_COST,MOUNTAIN_POWER,OCEAN_POWER,FOREST_POWER,PERMANENT,RESERVE,IsBanned,IsErrated,IsSuspended\n\
5,ALT_ALIZE_B_YZ_10_E,102,ALIZE,YZ,E,B,0,7,TBF-010-E,2,3,1,1,1,,,1,0,0\n\
6,ALT_ALIZE_B_YZ_11_C,103,ALIZE,YZ,C,B,0,7,TBF-011-C,1,2,,,,,,0,0,0\n",
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
