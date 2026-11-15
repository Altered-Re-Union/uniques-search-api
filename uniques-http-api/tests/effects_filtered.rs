use std::fs;
use std::path::PathBuf;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use roaring::RoaringBitmap;
use tower::ServiceExt;
use uniques_http_api::{app, load_index, ServerState};

// Builds a small on-disk index in a temp dir with per-line bitmaps so the
// /api/v2/effects/filtered semantics can be exercised end-to-end.
//
// Cards (card_index 0..3), per effect line:
//   card 0: M1 = {T1, C2, O3}                 (a full ability on M1)
//   card 1: M1 = {T1, O4}, M2 = {C5}          (T1+O4 share M1; C5 is on a different line)
//   card 2: M1 = {T7}, Ec = {T1, C2, O3}      (echo ability on Ec)
//   card 3: M1 = {T1, C2}                      (no output)
fn build_fixture() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("effects_filtered_{}", uuid::Uuid::new_v4()));
    let id_gd = dir.join("id_gd");
    fs::create_dir_all(&id_gd).unwrap();

    fs::write(
        dir.join("manifest.json"),
        r#"{"version":1,"set":"TEST","kind":"test","built_at_secs":0,"card_count":4,"id_gd_count":6,"total_bit_span":4,"family_count":1}"#,
    )
    .unwrap();

    fs::write(
        dir.join("catalog.json"),
        r#"{
  "set": "TEST",
  "faction_order": ["AX", "BR", "LY", "MU", "OR", "YZ"],
  "families": [
    {
      "start_bit": 0,
      "faction": "AX",
      "family_number": "04",
      "family_id": "AX_04",
      "max_unique_id": 4,
      "card_count": 4,
      "first_reference": "ALT_TEST_B_AX_04_U_1",
      "name": { "en_US": "Fixture Card" },
      "artist": "Fixture Artist",
      "card_sub_types": [],
      "set": { "reference": "COREKS", "name": "Beyond the Gates - KS Edition", "code": "BTG" }
    }
  ],
  "total_bit_span": 4
}"#,
    )
    .unwrap();

    fs::write(
        dir.join("stats_summary.json"),
        r#"{"version":1,"set":"TEST","total_cards_indexed":4,"fields":[]}"#,
    )
    .unwrap();

    fs::write(
        dir.join("factions_summary.json"),
        r#"{"version":1,"set":"TEST","total_cards_indexed":4,"source":"mainFaction.reference","factions":[],"unknown_count":0,"bitmap_dir":"factions"}"#,
    )
    .unwrap();

    fs::write(
        dir.join("idgd_catalog.json"),
        r#"{
  "set": "TEST",
  "entries": [
    { "id_gd": 1, "card_count": 4, "bitmap_bytes": 0, "bitmap_file": "1.roar", "element_type": "TRIGGER",
      "translations": { "en_US": { "locale": "en_US", "text": "{R}" } },
      "m1": { "card_count": 3, "bitmap_bytes": 1, "bitmap_file": "1_m1.roar" },
      "ec": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "1_ec.roar" },
      "is_main": true, "is_echo": true },
    { "id_gd": 2, "card_count": 3, "bitmap_bytes": 0, "bitmap_file": "2.roar", "element_type": "CONDITION",
      "translations": { "en_US": { "locale": "en_US", "text": "cond2" } },
      "m1": { "card_count": 2, "bitmap_bytes": 1, "bitmap_file": "2_m1.roar" },
      "ec": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "2_ec.roar" },
      "is_main": true, "is_echo": true },
    { "id_gd": 3, "card_count": 2, "bitmap_bytes": 0, "bitmap_file": "3.roar", "element_type": "OUTPUT",
      "translations": { "en_US": { "locale": "en_US", "text": "out3" } },
      "m1": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "3_m1.roar" },
      "ec": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "3_ec.roar" },
      "is_main": true, "is_echo": true },
    { "id_gd": 4, "card_count": 1, "bitmap_bytes": 0, "bitmap_file": "4.roar", "element_type": "OUTPUT",
      "translations": { "en_US": { "locale": "en_US", "text": "out4" } },
      "m1": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "4_m1.roar" },
      "is_main": true, "is_echo": false },
    { "id_gd": 5, "card_count": 1, "bitmap_bytes": 0, "bitmap_file": "5.roar", "element_type": "CONDITION",
      "translations": { "en_US": { "locale": "en_US", "text": "cond5" } },
      "m2": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "5_m2.roar" },
      "is_main": true, "is_echo": false },
    { "id_gd": 7, "card_count": 1, "bitmap_bytes": 0, "bitmap_file": "7.roar", "element_type": "TRIGGER",
      "translations": { "en_US": { "locale": "en_US", "text": "trig7" } },
      "m1": { "card_count": 1, "bitmap_bytes": 1, "bitmap_file": "7_m1.roar" },
      "is_main": true, "is_echo": false }
  ]
}"#,
    )
    .unwrap();

    // cards.bin must be total_bit_span * 32 bytes; contents are unused by this endpoint.
    fs::write(dir.join("cards.bin"), vec![0u8; 4 * 32]).unwrap();

    let write_roar = |name: &str, bits: &[u32]| {
        let mut bm = RoaringBitmap::new();
        for &b in bits {
            bm.insert(b);
        }
        let mut bytes = Vec::new();
        bm.serialize_into(&mut bytes).unwrap();
        fs::write(id_gd.join(name), bytes).unwrap();
    };

    write_roar("1_m1.roar", &[0, 1, 3]);
    write_roar("1_ec.roar", &[2]);
    write_roar("2_m1.roar", &[0, 3]);
    write_roar("2_ec.roar", &[2]);
    write_roar("3_m1.roar", &[0]);
    write_roar("3_ec.roar", &[2]);
    write_roar("4_m1.roar", &[1]);
    write_roar("5_m2.roar", &[1]);
    write_roar("7_m1.roar", &[2]);

    dir
}

fn test_server() -> ServerState {
    ServerState::for_test(load_index(&build_fixture()).expect("load fixture index"))
}

async fn call(server: ServerState, query: &str) -> (StatusCode, serde_json::Value) {
    let uri = if query.is_empty() {
        "/api/v2/effects/filtered".to_string()
    } else {
        format!("/api/v2/effects/filtered?{query}")
    };
    let response = app(server)
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    (status, value)
}

fn ids(value: &serde_json::Value) -> Vec<u64> {
    value["idGds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect()
}

// effect[0][t]=1 -> %5B/%5D encode the brackets so the query parses cleanly.
const T1: &str = "effect%5B0%5D%5Bt%5D=1";

#[tokio::test]
async fn lists_outputs_co_occurring_with_trigger_on_a_main_line() {
    let (status, body) = call(test_server(), &format!("{T1}&editing=output:0")).await;
    assert_eq!(status, 200);
    assert_eq!(body["editing"], "output:0");
    // O3 (with T1 on card 0's M1) and O4 (with T1 on card 1's M1).
    assert_eq!(ids(&body), vec![3, 4]);
}

#[tokio::test]
async fn same_line_constraint_excludes_cross_line_combinations() {
    // T1 is on M1 (cards 0,1,3); C5 is on M2 (card 1). No single main line has both,
    // so no output can complete an ability -> empty.
    let (status, body) = call(
        test_server(),
        &format!("{T1}&effect%5B0%5D%5Bc%5D=5&editing=output:0"),
    )
    .await;
    assert_eq!(status, 200);
    assert!(ids(&body).is_empty(), "expected no outputs, got {:?}", ids(&body));
}

#[tokio::test]
async fn support_region_uses_echo_line_only() {
    // support[t]=1 -> echo line Ec. Card 2's Ec has T1 and O3, but not O4 (main only).
    let (status, body) = call(test_server(), "support%5Bt%5D=1&editing=output:support").await;
    assert_eq!(status, 200);
    assert_eq!(body["editing"], "output:support");
    assert_eq!(ids(&body), vec![3]);
}

#[tokio::test]
async fn edited_group_is_excluded_from_base() {
    // Editing the trigger box whose own value is 7 must not filter out other triggers:
    // with no co-constraints, all main-line triggers are offered.
    let (status, body) = call(test_server(), "effect%5B0%5D%5Bt%5D=7&editing=trigger:0").await;
    assert_eq!(status, 200);
    assert_eq!(ids(&body), vec![1, 7]);
}

#[tokio::test]
async fn brand_new_slot_narrows_by_remaining_filters() {
    // editing=trigger:3 references a slot not present; effect[0] (T1) stays in Base, so only
    // triggers reachable among cards already matching T1 (cards 0,1,3) are offered -> just T1.
    let (status, body) = call(test_server(), &format!("{T1}&editing=trigger:3")).await;
    assert_eq!(status, 200);
    assert_eq!(ids(&body), vec![1]);
}

#[tokio::test]
async fn missing_editing_param_is_bad_request() {
    let (status, _) = call(test_server(), "").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_editing_part_is_bad_request() {
    let (status, _) = call(test_server(), "editing=banana:0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn invalid_editing_slot_is_bad_request() {
    let (status, _) = call(test_server(), "editing=trigger:nope").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn wrong_type_co_constraint_is_bad_request() {
    // effect[0][t]=3 -> id 3 is an OUTPUT, not a TRIGGER.
    let (status, _) = call(test_server(), "effect%5B0%5D%5Bt%5D=3&editing=output:0").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn has_no_effect_filters_effect_suggestions() {
    let (status_true, body_true) =
        call(test_server(), "hasNoEffect=true&editing=trigger:0").await;
    assert_eq!(status_true, 200);
    assert!(ids(&body_true).is_empty());

    let (status_false, body_false) =
        call(test_server(), "hasNoEffect=false&editing=trigger:0").await;
    assert_eq!(status_false, 200);
    assert_eq!(ids(&body_false), vec![1, 7]);
}
