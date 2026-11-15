use std::path::Path;

use axum::body::Body;
use http_body_util::BodyExt;
use tower::ServiceExt;
use uniques_http_api::{app, load_index, ServerState};

const FIXTURE_INDEX: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/minimal_index");

fn test_server() -> ServerState {
    ServerState::for_test(
        load_index(Path::new(FIXTURE_INDEX)).expect("load minimal test index"),
    )
}

async fn body_json(response: axum::response::Response) -> serde_json::Value {
    let body = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn post_cards_matches_get_cards_for_same_params() {
    let query = "ref[]=ALT_TEST_B_AX_04_U_1";

    let get_response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v2/cards?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(get_response.status(), 200);
    let get_body = body_json(get_response).await;

    let post_response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v2/cards")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(query))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(post_response.status(), 200);
    let post_body = body_json(post_response).await;

    assert_eq!(get_body, post_body);
    assert_eq!(post_body["iter"]["total"], 1);
    assert_eq!(post_body["cards"][0]["reference"], "ALT_TEST_B_AX_04_U_1");
}

#[tokio::test]
async fn post_cards_rejects_bad_params_same_as_get() {
    let query = "mainCost=99";

    let get_response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/api/v2/cards?{query}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let post_response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/api/v2/cards")
                .header("content-type", "application/x-www-form-urlencoded")
                .body(Body::from(query))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(get_response.status(), 400);
    assert_eq!(post_response.status(), 400);
}
