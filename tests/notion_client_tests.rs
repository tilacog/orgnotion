//! Tests for the notionrs-backed Notion adapter.
//!
//! `create_page` and `append_children` go through the `notionrs` crate,
//! which pins its request URLs to api.notion.com — those paths are
//! covered by the run-level tests against the `NotionApi` fake, and their
//! HTTP behavior is notionrs's contract. What *is* exercised here, against
//! an in-process stub server, is the adapter's own HTTP path: the
//! `list_children` workaround (see the adapter's module docs) and the
//! retry policy it shares with the notionrs-backed calls.

use notionrs_types::object::block::Block;
use orgnotion::adapters::NotionrsApi;
use orgnotion::converter::{page_mention, text_run};
use orgnotion::ports::{NotionApi, NotionError};
use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

/// One canned response: status line body pairs served in order; the
/// server records each request's first line + headers + body.
struct StubServer {
    base_url: String,
    requests: std::sync::mpsc::Receiver<String>,
    handle: thread::JoinHandle<()>,
}

fn serve(responses: Vec<String>) -> StubServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub server");
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = std::sync::mpsc::channel();

    let handle = thread::spawn(move || {
        for response in responses {
            let (mut stream, _) = listener.accept().expect("accept");
            let request = read_full_request(&mut stream);
            tx.send(request).ok();
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
    });

    StubServer {
        base_url: format!("http://127.0.0.1:{port}"),
        requests: rx,
        handle,
    }
}

/// Read headers plus (per Content-Length) the full body — the body often
/// arrives in a separate TCP segment from the headers.
fn read_full_request(stream: &mut std::net::TcpStream) -> String {
    let mut data = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let text = String::from_utf8_lossy(&data).to_string();
        if let Some(header_end) = text.find("\r\n\r\n") {
            let lower = text.to_lowercase();
            let content_length = lower
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if data.len() >= header_end + 4 + content_length {
                return text;
            }
        }
        let n = stream.read(&mut chunk).expect("read request");
        if n == 0 {
            return String::from_utf8_lossy(&data).to_string();
        }
        data.extend_from_slice(&chunk[..n]);
    }
}

fn http_response(status: u16, extra_headers: &str, body: &str) -> String {
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n{extra_headers}\r\n{body}",
        body.len()
    )
}

fn client(base_url: &str) -> NotionrsApi {
    let mut api = NotionrsApi::with_base_url("test-token".to_string(), base_url.to_string());
    api.set_backoff_base_ms(1);
    api
}

/// A children-list entry as Notion serves it: listing metadata merged
/// with the typed block's serialized form.
fn child_json(id: &str, has_children: bool, block: &Block) -> Value {
    let mut merged = json!({
        "object": "block",
        "id": id,
        "has_children": has_children,
    });
    let block_json = serde_json::to_value(block).unwrap();
    merged
        .as_object_mut()
        .unwrap()
        .extend(block_json.as_object().unwrap().clone());
    merged
}

fn paragraph(rich_text: Vec<notionrs_types::object::rich_text::RichText>) -> Block {
    Block::Paragraph {
        paragraph: notionrs_types::object::block::paragraph::ParagraphBlock::default()
            .rich_text(rich_text),
    }
}

#[tokio::test]
async fn list_children_sends_auth_version_and_cursor_and_parses_typed_blocks() {
    let body = json!({
        "results": [child_json(
            "b1",
            true,
            &paragraph(vec![text_run("hello "), page_mention("page-b")]),
        )],
        "has_more": true,
        "next_cursor": "cursor-xyz"
    });
    let server = serve(vec![http_response(200, "", &body.to_string())]);
    let page = client(&server.base_url)
        .list_children("block-1", Some("cursor-abc"))
        .await
        .unwrap();

    assert_eq!(page.blocks.len(), 1);
    assert_eq!(page.blocks[0].id, "b1");
    assert!(page.blocks[0].has_children);
    assert!(matches!(page.blocks[0].block, Block::Paragraph { .. }));
    assert_eq!(page.next_cursor.as_deref(), Some("cursor-xyz"));

    let request = server.requests.recv().unwrap();
    assert!(
        request.starts_with("GET /blocks/block-1/children"),
        "got: {request}"
    );
    assert!(
        request.contains("start_cursor=cursor-abc"),
        "got: {request}"
    );
    assert!(request.contains("Bearer test-token"), "got: {request}");
    assert!(
        request.contains("notion-version: 2026-03-11")
            || request.contains("Notion-Version: 2026-03-11"),
        "got: {request}"
    );
    server.handle.join().unwrap();
}

#[tokio::test]
async fn list_children_without_more_has_no_cursor() {
    let body = json!({"results": [], "has_more": false, "next_cursor": null});
    let server = serve(vec![http_response(200, "", &body.to_string())]);
    let page = client(&server.base_url)
        .list_children("b", None)
        .await
        .unwrap();
    assert!(page.blocks.is_empty());
    assert!(page.next_cursor.is_none());
    server.handle.join().unwrap();
}

#[tokio::test]
async fn retries_on_429_honoring_retry_after_then_succeeds() {
    let ok = json!({"results": [], "has_more": false, "next_cursor": null});
    let server = serve(vec![
        http_response(429, "Retry-After: 0\r\n", "{}"),
        http_response(200, "", &ok.to_string()),
    ]);
    let page = client(&server.base_url)
        .list_children("b", None)
        .await
        .unwrap();
    assert!(page.blocks.is_empty());
    // Both requests reached the server.
    server.requests.recv().unwrap();
    server.requests.recv().unwrap();
    server.handle.join().unwrap();
}

#[tokio::test]
async fn retries_on_500_then_gives_up_with_api_error() {
    let responses = vec![http_response(500, "", r#"{"message":"boom"}"#); 5];
    let server = serve(responses);
    let err = client(&server.base_url)
        .list_children("b", None)
        .await
        .unwrap_err();
    match err {
        NotionError::Api { status, body } => {
            assert_eq!(status, 500);
            assert!(body.contains("boom"));
        }
        other => panic!("expected Api error, got {other}"),
    }
    server.handle.join().unwrap();
}

#[tokio::test]
async fn auth_failure_is_not_retried_and_hints_at_token() {
    let server = serve(vec![http_response(
        401,
        "",
        r#"{"message":"unauthorized"}"#,
    )]);
    let err = client(&server.base_url)
        .list_children("b", None)
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("401"), "got: {message}");
    assert!(message.contains("NOTION_TOKEN"), "got: {message}");
    server.handle.join().unwrap(); // only one request was served
}

#[tokio::test]
async fn malformed_success_response_is_an_unexpected_shape_error() {
    let server = serve(vec![http_response(200, "", r#"{"no_results_here": true}"#)]);
    let err = client(&server.base_url)
        .list_children("b", None)
        .await
        .unwrap_err();
    assert!(matches!(err, NotionError::UnexpectedShape(_)));
    server.handle.join().unwrap();
}

#[tokio::test]
async fn transport_failure_surfaces_after_retries() {
    // Nothing listening on this port.
    let mut api = NotionrsApi::with_base_url("t".to_string(), "http://127.0.0.1:1".to_string());
    api.set_backoff_base_ms(1);
    let err = api.list_children("b", None).await.unwrap_err();
    assert!(matches!(err, NotionError::Transport(_)));
}
