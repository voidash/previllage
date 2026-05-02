//! Tests for chunks persistence via Store::insert_chunks +
//! list_unchunked_documents.

use chrono::Utc;
use gemma_god::crawler_v2::chunk::Chunk;
use gemma_god::crawler_v2::types::{DocType, Document};
use gemma_god::crawler_v2::Store;
use gemma_god::crawler_v2::types::RegistryRow;

fn seed_source(store: &Store, sid: &str) {
    store
        .upsert_source_from_registry(
            &RegistryRow {
                source_id: sid.into(),
                domain: format!("{sid}.gov.np"),
                homepage_url: format!("https://{sid}.gov.np/"),
                name_en: None,
                name_np: None,
                office_type: None,
                province: None,
                tier: 2,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

fn seed_doc(store: &mut Store, sid: &str, url: &str, hash: &str) -> String {
    let doc_id = format!("{sid}__{hash}");
    let doc = Document {
        doc_id: doc_id.clone(),
        source_id: sid.into(),
        url: url.into(),
        content_hash: hash.into(),
        fetched_at: Utc::now(),
        superseded_by: None,
        removed_at: None,
        doc_type: DocType::Html,
        status_code: 200,
        title: None,
        language: None,
        date_published: None,
        raw_blob_path: format!("raw/{sid}/{hash}.html"),
        extracted_text_path: Some(format!("extracted/{sid}/{hash}.txt")),
        text_chars: 500,
        size_bytes: 2048,
        depth: 0,
        priority_at_fetch: Some(100),
    };
    store.upsert_document(&doc).unwrap();
    doc_id
}

fn chunk(id: &str, idx: u32, text: &str) -> Chunk {
    Chunk {
        chunk_id: id.into(),
        chunk_index: idx,
        text: text.into(),
        char_start: 0,
        char_end: text.chars().count() as u32,
    }
}

#[test]
fn insert_and_count_chunks() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let doc_id = seed_doc(&mut store, "moha", "https://moha.gov.np/a", "hash1");

    let chunks = vec![
        chunk("c1", 0, "hello"),
        chunk("c2", 1, "world"),
    ];
    let n = store
        .insert_chunks(&doc_id, &chunks, &["devanagari", "latin"], Utc::now())
        .unwrap();
    assert_eq!(n, 2);
    assert_eq!(store.chunk_count_total().unwrap(), 2);
    assert_eq!(store.chunk_count_for_doc(&doc_id).unwrap(), 2);
}

#[test]
fn re_insert_same_chunks_is_noop() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let doc_id = seed_doc(&mut store, "moha", "https://moha.gov.np/a", "hash1");

    let chunks = vec![chunk("c1", 0, "hello"), chunk("c2", 1, "world")];
    store
        .insert_chunks(&doc_id, &chunks, &["latin", "latin"], Utc::now())
        .unwrap();
    // Second call with identical chunk_ids → INSERT OR IGNORE returns 0.
    let n2 = store
        .insert_chunks(&doc_id, &chunks, &["latin", "latin"], Utc::now())
        .unwrap();
    assert_eq!(n2, 0);
    assert_eq!(store.chunk_count_total().unwrap(), 2);
}

#[test]
fn list_unchunked_documents_excludes_already_chunked() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let a = seed_doc(&mut store, "moha", "https://moha.gov.np/a", "hash_a");
    let b = seed_doc(&mut store, "moha", "https://moha.gov.np/b", "hash_b");

    // Chunk doc a only.
    store
        .insert_chunks(&a, &[chunk("ca1", 0, "hi")], &["latin"], Utc::now())
        .unwrap();

    let unchunked = store.list_unchunked_documents(None, None).unwrap();
    assert_eq!(unchunked.len(), 1);
    assert_eq!(unchunked[0].doc_id, b);
}

#[test]
fn limit_bounds_returned_documents() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    for i in 0..5 {
        seed_doc(&mut store, "moha", &format!("https://moha.gov.np/{i}"), &format!("h{i}"));
    }
    let limited = store.list_unchunked_documents(Some(3), None).unwrap();
    assert_eq!(limited.len(), 3);
}
