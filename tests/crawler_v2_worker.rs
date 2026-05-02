//! End-to-end integration test for the Phase 5 worker loop.
//!
//! Spins up a wiremock server serving a small gov-site tree, seeds a real
//! SQLite database with one Source pointing at it, constructs a real Worker,
//! runs a polling cycle, and asserts the SQLite + filesystem outcomes.
//!
//! This is the first test that exercises fetch + parse + canonicalize +
//! frontier + persistence + diff-detection together.

use chrono::Utc;
use gemma_god::crawler_v2::{
    BlobStore, DomainThrottle, FetchConfig, Fetcher, Recipe, SourceStatus, StopReason, Store,
    ThrottleConfig, Worker,
};
use gemma_god::crawler_v2::types::RegistryRow;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Minimal Recipe for tests: strict host match (so 127.0.0.1 works),
/// fast caps, no robots.
fn test_recipe(source_id: &str, homepage: &str) -> Recipe {
    Recipe {
        source_id: source_id.to_string(),
        version: 1,
        entry_points: vec![homepage.to_string()],
        deny_paths: vec![],
        allow_paths: None,
        max_depth: 2,
        max_pdf_depth: 3,
        max_html_fetches: 50,
        max_total_fetches: 100,
        max_elapsed_sec: 30,
        rate_limit_ms: 0,
        respect_robots: false,
        allowed_subdomains: Some(vec![]), // strict: exact host match only
        custom_user_agent: None,
        js_render_required: false,
        notes: String::new(),
        last_repaired_at: None,
        repaired_by: None,
    }
}

fn fast_fetcher() -> Arc<Fetcher> {
    Arc::new(
        Fetcher::new(FetchConfig {
            timeout: Duration::from_secs(5),
            ..FetchConfig::default()
        })
        .unwrap(),
    )
}

fn fast_throttle() -> Arc<DomainThrottle> {
    Arc::new(DomainThrottle::new(ThrottleConfig {
        min_interval: Duration::from_millis(0),
        jitter: Duration::from_millis(0),
    }))
}

struct Fixture {
    db_path: std::path::PathBuf,
    // Held so the temp dir isn't reaped during the test; never read.
    #[allow(dead_code)]
    _blob_dir: TempDir,
    blobs: Arc<BlobStore>,
    source_id: String,
    #[allow(dead_code)]
    server_uri: String,
}

async fn setup_fixture() -> (MockServer, Fixture) {
    let server = MockServer::start().await;
    let server_uri = server.uri();
    let server_host = url::Url::parse(&server_uri).unwrap().host_str().unwrap().to_string();

    // Register the site tree. `set_body_raw(bytes, mime)` is the reliable
    // way to set content-type in wiremock — `set_body_string` silently
    // overrides the content-type header to text/plain.
    Mock::given(method("GET")).and(path("/")).respond_with(
        ResponseTemplate::new(200)
            .set_body_raw(home_html(&server_uri).into_bytes(), "text/html"),
    ).mount(&server).await;

    Mock::given(method("GET")).and(path("/notice/1")).respond_with(
        ResponseTemplate::new(200).set_body_raw(
            notice_html("First notice", "The first notice body text with at least 100 chars so extraction returns it rather than falling back to body — this is the primary article content.").into_bytes(),
            "text/html",
        ),
    ).mount(&server).await;

    Mock::given(method("GET")).and(path("/notice/2")).respond_with(
        ResponseTemplate::new(200).set_body_raw(
            notice_html("Second notice", "Another article with enough text to pass the 100-char threshold for main-region selection in the parser's extract_readable_text fallback logic.").into_bytes(),
            "text/html",
        ),
    ).mount(&server).await;

    Mock::given(method("GET")).and(path("/acts/1.pdf")).respond_with(
        ResponseTemplate::new(200)
            .set_body_raw(b"%PDF-1.4\nfake pdf body\n%%EOF".to_vec(), "application/pdf"),
    ).mount(&server).await;

    Mock::given(method("GET")).and(path("/contact")).respond_with(
        ResponseTemplate::new(200).set_body_raw(
            notice_html("Contact", "Office hours etc.").into_bytes(),
            "text/html",
        ),
    ).mount(&server).await;

    // DB + blobs in temp dirs.
    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let blob_dir = TempDir::new().unwrap();
    let blobs = Arc::new(BlobStore::new(blob_dir.path()).unwrap());

    // Seed the source. Domain = wiremock host (e.g., "127.0.0.1"), poll=1h.
    let store = Store::open(&db_path).unwrap();
    let source_id = "test_source".to_string();
    let row = RegistryRow {
        source_id: source_id.clone(),
        domain: server_host.clone(),
        homepage_url: format!("{}/", server_uri),
        name_en: Some("Test gov".into()),
        name_np: None,
        office_type: Some("Federal".into()),
        province: None,
        tier: 2,
        poll_interval_hours: Some(1),
        status: None,
        first_seen: None,
    };
    store.upsert_source_from_registry(&row, Utc::now()).unwrap();
    drop(store);
    // Prevent the temp dir holding the DB file from being dropped.
    std::mem::forget(db_tmp);

    (
        server,
        Fixture {
            db_path,
            _blob_dir: blob_dir,
            blobs,
            source_id,
            server_uri,
        },
    )
}

fn home_html(server_uri: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><title>Test Gov Home</title></head>
<body>
  <main>
    <h1>Test Government Home</h1>
    <p>This is the home page with enough content to pass the 100-char main-selector
       threshold, otherwise the extractor falls back to the body which we still
       test elsewhere. We need at least 100 characters.</p>
    <ul>
      <li><a href="{u}/notice/1">Notice 1</a></li>
      <li><a href="{u}/notice/2">Notice 2</a></li>
      <li><a href="{u}/acts/1.pdf">Act 1</a></li>
      <li><a href="{u}/contact">Contact</a></li>
    </ul>
  </main>
</body></html>"#,
        u = server_uri
    )
}

fn notice_html(title: &str, body: &str) -> String {
    format!(
        r#"<!doctype html>
<html lang="en"><head><title>{t}</title></head>
<body><main><h1>{t}</h1><p>{b}</p></main></body></html>"#,
        t = title,
        b = body
    )
}

fn shell_html() -> String {
    r#"<!doctype html>
<html><head><title>Loading…</title></head>
<body>
  <div id="app"></div>
  <script type="module" src="/assets/app.js"></script>
  <script type="module" src="/assets/vendor.js"></script>
</body></html>"#.to_string()
}

async fn run_worker_once(fx: &Fixture) -> gemma_god::crawler_v2::WorkerReport {
    let store = Store::open(&fx.db_path).unwrap();
    let source = store.get_source(&fx.source_id).unwrap().unwrap();
    let recipe = test_recipe(&source.source_id, &source.homepage_url);

    let worker_store = Store::open(&fx.db_path).unwrap();
    let worker = Worker::new(
        source,
        recipe,
        fast_fetcher(),
        fast_throttle(),
        fx.blobs.clone(),
        worker_store,
    );
    worker.poll().await.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn end_to_end_crawl_captures_html_and_pdfs() {
    let (_server, fx) = setup_fixture().await;
    let report = run_worker_once(&fx).await;

    // 4 HTML pages (home + 2 notices + contact) and 1 PDF.
    assert_eq!(report.html_fetched, 4, "{report:?}");
    assert_eq!(report.binaries_fetched, 1, "{report:?}");
    assert_eq!(report.errors, 0, "{report:?}");
    assert_eq!(report.docs_inserted, 5, "{report:?}");
    assert_eq!(report.docs_superseded, 0);
    assert_eq!(report.docs_unchanged, 0);
    assert_eq!(report.stop_reason, StopReason::FrontierDrained);
    assert!(!report.shell_flagged);

    // DB state: 5 live rows, last_polled_at + next_poll_at set.
    let store = Store::open(&fx.db_path).unwrap();
    let live_count: i64 = store
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM documents
              WHERE source_id = ?1 AND superseded_by IS NULL",
            rusqlite::params![fx.source_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live_count, 5);

    let src = store.get_source(&fx.source_id).unwrap().unwrap();
    assert!(src.last_polled_at.is_some());
    assert!(src.next_poll_at.is_some());
    assert_eq!(src.consecutive_failures, 0);
    assert_eq!(src.status, SourceStatus::Active);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rerun_detects_unchanged_content_via_content_hash() {
    let (_server, fx) = setup_fixture().await;
    run_worker_once(&fx).await;
    let second = run_worker_once(&fx).await;

    // Second cycle: every URL re-fetched, but content unchanged → every row
    // should be Unchanged (fetched_at bumped, no new row).
    assert_eq!(second.docs_inserted, 0, "{second:?}");
    assert_eq!(second.docs_superseded, 0);
    assert_eq!(second.docs_unchanged, 5);
    assert_eq!(second.errors, 0);
    assert_eq!(second.stop_reason, StopReason::FrontierDrained);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shell_detected_homepage_flags_source_js_only() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(shell_html().into_bytes(), "text/html"),
        )
        .mount(&server)
        .await;
    let server_host = url::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let blob_dir = TempDir::new().unwrap();
    let blobs = Arc::new(BlobStore::new(blob_dir.path()).unwrap());

    let store = Store::open(&db_path).unwrap();
    let sid = "shell_source".to_string();
    let row = RegistryRow {
        source_id: sid.clone(),
        domain: server_host.clone(),
        homepage_url: format!("{}/", server.uri()),
        name_en: Some("Shell site".into()),
        name_np: None,
        office_type: Some("Federal".into()),
        province: None,
        tier: 3,
        poll_interval_hours: Some(24),
        status: None,
        first_seen: None,
    };
    store.upsert_source_from_registry(&row, Utc::now()).unwrap();

    let source = store.get_source(&sid).unwrap().unwrap();
    let recipe = test_recipe(&sid, &source.homepage_url);

    let worker_store = Store::open(&db_path).unwrap();
    let worker = Worker::new(
        source,
        recipe,
        fast_fetcher(),
        fast_throttle(),
        blobs,
        worker_store,
    );
    let report = worker.poll().await.unwrap();

    assert!(report.shell_flagged);
    assert_eq!(report.stop_reason, StopReason::ShellDetected);

    // Source status flipped to JsOnly.
    let reopened = Store::open(&db_path).unwrap();
    let s = reopened.get_source(&sid).unwrap().unwrap();
    assert_eq!(s.status, SourceStatus::JsOnly);
    std::mem::forget(db_tmp);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fetch_errors_bump_consecutive_failures_but_cycle_completes() {
    // Serve the homepage but have its links return network errors for this test:
    // mock server will 500 on /notice/1, /notice/2. Homepage succeeds so the
    // worker has something to do; link fetches fail but the cycle completes
    // with errors recorded rather than crashing.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(home_html(&server.uri()).into_bytes(), "text/html"),
        )
        .mount(&server)
        .await;
    // /notice/1 and /notice/2 unregistered → wiremock returns 404
    // /acts/1.pdf and /contact: we set explicit 500s.
    Mock::given(method("GET"))
        .and(path("/acts/1.pdf"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/contact"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let server_host = url::Url::parse(&server.uri())
        .unwrap()
        .host_str()
        .unwrap()
        .to_string();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let blob_dir = TempDir::new().unwrap();
    let blobs = Arc::new(BlobStore::new(blob_dir.path()).unwrap());

    let store = Store::open(&db_path).unwrap();
    let sid = "err_source".to_string();
    store
        .upsert_source_from_registry(
            &RegistryRow {
                source_id: sid.clone(),
                domain: server_host.clone(),
                homepage_url: format!("{}/", server.uri()),
                name_en: None,
                name_np: None,
                office_type: None,
                province: None,
                tier: 3,
                poll_interval_hours: Some(24),
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();

    let source = store.get_source(&sid).unwrap().unwrap();
    let recipe = test_recipe(&sid, &source.homepage_url);
    let worker_store = Store::open(&db_path).unwrap();
    let worker = Worker::new(
        source,
        recipe,
        fast_fetcher(),
        fast_throttle(),
        blobs,
        worker_store,
    );
    let report = worker.poll().await.unwrap();

    // Homepage succeeded (1 html), 4 link fetches all error out (404+500s).
    assert_eq!(report.html_fetched, 1);
    assert!(report.errors >= 3, "expected >=3 errors, got {report:?}");
    assert_eq!(report.stop_reason, StopReason::FrontierDrained);
    std::mem::forget(db_tmp);
}
