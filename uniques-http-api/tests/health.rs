use std::io::Read;
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

#[tokio::test]
async fn health_returns_hello_world_json() {
    let response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        body,
        br#"{"message":"Hello World"}"#.as_ref()
    );
}

#[tokio::test]
async fn cors_allows_any_origin() {
    let response = app(test_server())
        .oneshot(
            axum::http::Request::builder()
                .uri("/healthz")
                .header(axum::http::header::ORIGIN, "https://example.com")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(
        response
            .headers()
            .get(axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN)
            .and_then(|v| v.to_str().ok()),
        Some("*")
    );
}

async fn get(uri: &str, accept_gzip: bool) -> (axum::http::HeaderMap, Vec<u8>) {
    let mut builder = axum::http::Request::builder().uri(uri);
    if accept_gzip {
        builder = builder.header(axum::http::header::ACCEPT_ENCODING, "gzip");
    }
    let response = app(test_server())
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "{uri}");
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (headers, body.to_vec())
}

fn gunzip(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(body)
        .read_to_end(&mut out)
        .expect("valid gzip body");
    out
}

#[tokio::test]
async fn effects_list_is_gzipped_when_the_client_accepts_it() {
    // `/api/v2/effects` is gzipped once at load.
    for uri in ["/api/v2/effects"] {
        let (plain_headers, plain_body) = get(uri, false).await;
        assert!(
            plain_headers
                .get(axum::http::header::CONTENT_ENCODING)
                .is_none(),
            "{uri}"
        );
        let plain: serde_json::Value = serde_json::from_slice(&plain_body).unwrap();

        let (gzip_headers, gzip_body) = get(uri, true).await;
        assert_eq!(
            gzip_headers
                .get(axum::http::header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip"),
            "{uri}"
        );
        let decoded: serde_json::Value = serde_json::from_slice(&gunzip(&gzip_body)).unwrap();
        assert_eq!(decoded, plain, "{uri}");
    }
}
