//! Static file serving (`mudd --static-dir`)
//!
//! With --static-dir, mudd serves the built web client from the same origin
//! as the API: files by path, index.html for `/` and unknown client routes.

use crate::harness::TestServer;

#[tokio::test]
async fn test_static_dir_serves_client_with_spa_fallback() {
    let site = tempfile::TempDir::new().expect("temp dir");
    std::fs::write(
        site.path().join("index.html"),
        "<!doctype html><title>Mudcroft</title>",
    )
    .unwrap();
    std::fs::create_dir(site.path().join("assets")).unwrap();
    std::fs::write(site.path().join("assets/app.js"), "console.log('hi')").unwrap();

    let dir = site.path().to_str().unwrap();
    let server = TestServer::start_with_args(false, &["--static-dir", dir])
        .await
        .expect("server with static dir");

    let index = server.get("/").await.unwrap();
    assert_eq!(index.status(), 200);
    assert!(
        index
            .text()
            .await
            .unwrap()
            .contains("<title>Mudcroft</title>")
    );

    let js = server.get("/assets/app.js").await.unwrap();
    assert_eq!(js.status(), 200);
    let ctype = js.headers()["content-type"].to_str().unwrap().to_string();
    assert!(ctype.contains("javascript"), "content-type: {}", ctype);

    // Unknown non-API paths fall back to the client
    let spa = server.get("/some/client/route").await.unwrap();
    assert_eq!(spa.status(), 200);
    assert!(
        spa.text()
            .await
            .unwrap()
            .contains("<title>Mudcroft</title>")
    );

    // The API is unaffected
    let health = server.get("/health").await.unwrap();
    assert_eq!(health.status(), 200);
    let login = server
        .post(
            "/auth/login",
            &serde_json::json!({"username": "nobody", "password": "wrong"}),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), 401);
}

#[tokio::test]
async fn test_static_dir_without_index_is_rejected() {
    let empty = tempfile::TempDir::new().expect("temp dir");
    let dir = empty.path().to_str().unwrap();
    let result = TestServer::start_with_args(false, &["--static-dir", dir]).await;
    let err = result.err().expect("startup must fail").to_string();
    assert!(
        err.contains("index.html"),
        "error names the problem: {}",
        err
    );
}

/// mudd serves the client directly (no proxy in front), so it compresses
/// text and sets the browser security headers itself
#[tokio::test]
async fn test_static_dir_gzip_and_security_headers() {
    let site = tempfile::TempDir::new().expect("temp dir");
    let big = format!(
        "<!doctype html><title>Mudcroft</title>{}",
        "<p>cave</p>".repeat(200)
    );
    std::fs::write(site.path().join("index.html"), &big).unwrap();
    let dir = site.path().to_str().unwrap();
    let server = TestServer::start_with_args(false, &["--static-dir", dir])
        .await
        .expect("server with static dir");

    let resp = reqwest::Client::new()
        .get(format!("{}/", server.base_url()))
        .header("Accept-Encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let h = resp.headers();
    assert_eq!(
        h.get("content-encoding").map(|v| v.to_str().unwrap()),
        Some("gzip")
    );
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["x-frame-options"], "SAMEORIGIN");
    assert_eq!(h["referrer-policy"], "strict-origin-when-cross-origin");
    assert!(resp.bytes().await.unwrap().len() < big.len());

    // API responses carry the headers too
    let health = server.get("/health").await.unwrap();
    assert_eq!(health.headers()["x-content-type-options"], "nosniff");
}
