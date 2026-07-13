//! End-to-end tests of the run orchestration, entirely against in-memory
//! fakes — no network, no real filesystem, no real clock or env.

mod common;

use common::{CollectingReporter, FakeEnv, FakeNotion, FixedClock, InMemoryFileSystem};
use orgnotion::notion::MAX_CHILDREN_PER_REQUEST;
use orgnotion::run::{PARENT_PAGE_ID_ENV_VAR, RunConfig, RunError, execute};
use std::path::PathBuf;

const VAULT: &str = "/vault";

fn org(id: &str, title: &str, body: &str) -> String {
    format!(":PROPERTIES:\n:ID: {id}\n:END:\n#+TITLE: {title}\n\n{body}\n")
}

fn config() -> RunConfig {
    RunConfig {
        vault_dir: PathBuf::from(VAULT),
        parent_page_id: Some("parent-page".to_string()),
        title: None,
        dry_run: false,
        concurrency: 4,
    }
}

fn two_node_vault() -> InMemoryFileSystem {
    InMemoryFileSystem::with_files(&[
        (
            "/vault/a.org",
            &org("a", "Node A", "Links to [[id:b][Node B]]."),
        ),
        ("/vault/b.org", &org("b", "Node B", "Plain text only.")),
    ])
}

async fn run_with(
    cfg: &RunConfig,
    fs: &InMemoryFileSystem,
    notion: &FakeNotion,
) -> Result<orgnotion::run::RunReport, RunError> {
    execute(
        cfg,
        fs,
        notion,
        &FixedClock::at("2026-07-12T14:03:00Z"),
        &FakeEnv::default(),
        &mut CollectingReporter::default(),
    )
    .await
}

#[tokio::test]
async fn happy_path_creates_root_then_pages_then_content() {
    let notion = FakeNotion::new();
    let report = run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap();

    assert_eq!(report.node_count, 2);
    assert!(report.block_count >= 2);
    assert_eq!(
        report.root_url.as_deref(),
        Some("https://www.notion.so/page-0")
    );

    let pages = notion.pages();
    assert_eq!(pages.len(), 3); // root + 2 nodes
    assert_eq!(pages[0].parent_id, "parent-page");
    assert!(pages[1..].iter().all(|p| p.parent_id == pages[0].id));
    let titles: Vec<_> = pages[1..].iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, vec!["Node A", "Node B"]);
}

#[tokio::test]
async fn nested_vault_mirrors_directory_tree() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/hub.org", &org("hub", "Hub", "Root-level node.")),
        (
            "/vault/backend/autopilot/a.org",
            &org("a", "Autopilot A", "Body."),
        ),
        (
            "/vault/backend/indexer/b.org",
            &org("b", "Indexer B", "Body."),
        ),
    ]);
    let report = run_with(&config(), &fs, &notion).await.unwrap();
    assert_eq!(report.node_count, 3);

    let pages = notion.pages();
    assert_eq!(pages.len(), 7); // root + 3 directories + 3 nodes
    let root = &pages[0];
    let find = |title: &str| {
        pages
            .iter()
            .find(|p| p.title == title)
            .unwrap_or_else(|| panic!("no page titled {title:?}"))
    };

    let backend = find("backend");
    let autopilot = find("autopilot");
    let indexer = find("indexer");
    assert_eq!(backend.parent_id, root.id);
    assert_eq!(autopilot.parent_id, backend.id);
    assert_eq!(indexer.parent_id, backend.id);

    assert_eq!(find("Hub").parent_id, root.id);
    assert_eq!(find("Autopilot A").parent_id, autopilot.id);
    assert_eq!(find("Indexer B").parent_id, indexer.id);
}

#[tokio::test]
async fn sibling_pages_are_created_in_sorted_order_even_when_requests_lag() {
    let mut notion = FakeNotion::new();
    // The first node-page create yields to the executor before recording;
    // concurrent creation would record later requests first and scramble
    // sibling order in Notion, which shows children in creation order.
    notion.stagger_creates = true;
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/1.org", &org("n1", "1 One", "Body.")),
        ("/vault/2.org", &org("n2", "2 Two", "Body.")),
        ("/vault/3.org", &org("n3", "3 Three", "Body.")),
        ("/vault/4.org", &org("n4", "4 Four", "Body.")),
    ]);
    run_with(&config(), &fs, &notion).await.unwrap();

    let titles: Vec<_> = notion.pages()[1..]
        .iter()
        .map(|p| p.title.clone())
        .collect();
    assert_eq!(titles, vec!["1 One", "2 Two", "3 Three", "4 Four"]);
}

/// The `(type, plain text)` of every rich-text-bearing block on a page,
/// flattened — enough to assert concatenation order.
fn page_texts(notion: &FakeNotion, page_id: &str) -> Vec<(String, String)> {
    let children = serde_json::to_value(notion.children_of(page_id)).unwrap();
    children
        .as_array()
        .unwrap()
        .iter()
        .map(|b| {
            let kind = b["type"].as_str().unwrap().to_string();
            let text = b[&kind]["rich_text"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|rt| rt["text"]["content"].as_str())
                .collect::<String>();
            (kind, text)
        })
        .collect()
}

#[tokio::test]
async fn unreviewed_node_gets_warning_callout_as_first_block() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        (
            "/vault/a.org",
            ":PROPERTIES:\n:ID: a\n:END:\n#+TITLE: Node A\n#+filetags: :unreviewed:\n\n* First heading\n\nBody.\n",
        ),
        ("/vault/b.org", &org("b", "Node B", "* Heading B\n\nFine.")),
    ]);
    run_with(&config(), &fs, &notion).await.unwrap();

    let pages = notion.pages();
    let page_of = |title: &str| {
        pages
            .iter()
            .find(|p| p.title == title)
            .unwrap_or_else(|| panic!("no page titled {title}"))
            .id
            .clone()
    };

    let children = serde_json::to_value(notion.children_of(&page_of("Node A"))).unwrap();
    let children = children.as_array().unwrap();
    assert_eq!(children[0]["type"], "callout");
    assert_eq!(
        children[0]["callout"]["rich_text"][0]["text"]["content"],
        "This content hasn't been reviewed yet, proceed with caution"
    );
    assert_eq!(
        children[0]["callout"]["rich_text"][0]["annotations"]["color"],
        "red"
    );
    assert_eq!(children[0]["callout"]["icon"]["emoji"], "⚠️");
    assert_eq!(children[1]["type"], "heading_1");

    // The untagged node is untouched: normal heading color, no callout.
    let kinds_b = page_texts(&notion, &page_of("Node B"));
    assert!(kinds_b.iter().all(|(kind, _)| kind != "callout"));
}

#[tokio::test]
async fn flat_index_dir_merges_files_onto_the_index_page() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        // Declared out of name order; concatenation must sort by file
        // name — except the index node, which always leads even though
        // "z-index.org" sorts last.
        ("/vault/merged/b.org", &org("b", "Title B", "Content of B.")),
        (
            "/vault/merged/a.org",
            &org("a", "Title A", "Content of A. See [[id:b][B]]."),
        ),
        (
            "/vault/merged/z-index.org",
            &org("i", "Merged Index", "Index intro."),
        ),
        ("/vault/merged/.INDEX", "z-index.org\nflat = true\n"),
        (
            "/vault/outside.org",
            &org("out", "Outside", "Points into [[id:a][A]]."),
        ),
    ]);
    let report = run_with(&config(), &fs, &notion).await.unwrap();
    assert_eq!(report.node_count, 4);

    // Root + the merged directory page + the one regular node page. The
    // directory page takes the index node's title, not the dir basename.
    let pages = notion.pages();
    assert_eq!(pages.len(), 3);
    let root = &pages[0];
    let merged = pages.iter().find(|p| p.title == "Merged Index").unwrap();
    let outside = pages.iter().find(|p| p.title == "Outside").unwrap();
    assert_eq!(merged.parent_id, root.id);
    assert!(!pages.iter().any(|p| p.title == "merged"));
    assert!(!pages.iter().any(|p| p.title == "Title A"));

    // Index content first with no title heading (the page name carries
    // its title), then title heading + body per file in file-name order.
    let texts = page_texts(&notion, &merged.id);
    let kinds: Vec<&str> = texts.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "paragraph",
            "heading_1",
            "paragraph",
            "heading_1",
            "paragraph"
        ]
    );
    assert_eq!(texts[0].1, "Index intro.");
    assert_eq!(texts[1].1, "Title A");
    assert!(texts[2].1.starts_with("Content of A."), "got: {texts:?}");
    assert_eq!(texts[3].1, "Title B");
    assert_eq!(texts[4].1, "Content of B.");

    // A link into the merged dir mentions the merged page.
    let outside_content = serde_json::to_value(notion.children_of(&outside.id)).unwrap();
    let mentions: Vec<_> = outside_content
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["paragraph"]["rich_text"].as_array())
        .flatten()
        .filter(|rt| rt["type"] == "mention")
        .collect();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0]["mention"]["page"]["id"], merged.id.as_str());

    // Post-validation fetched the merged page once, not once per node.
    let merged_lists = notion
        .list_calls()
        .iter()
        .filter(|(id, _)| id == &merged.id)
        .count();
    assert_eq!(merged_lists, 1);
}

#[tokio::test]
async fn non_flat_index_renders_on_the_directory_page_with_child_pages() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/sub/index.org", &org("i", "Sub Index", "Overview.")),
        ("/vault/sub/a.org", &org("a", "Title A", "Content of A.")),
        ("/vault/sub/.INDEX", "index.org\nflat = false\n"),
    ]);
    let report = run_with(&config(), &fs, &notion).await.unwrap();
    assert_eq!(report.node_count, 2);

    // Root + the directory page (titled by the index node) + one child
    // page for the non-index node. The index node has no page of its own.
    let pages = notion.pages();
    assert_eq!(pages.len(), 3);
    let dir_page = pages.iter().find(|p| p.title == "Sub Index").unwrap();
    let child = pages.iter().find(|p| p.title == "Title A").unwrap();
    assert_eq!(dir_page.parent_id, pages[0].id);
    assert_eq!(child.parent_id, dir_page.id);

    // The index content sits on the directory page, without a heading.
    let texts = page_texts(&notion, &dir_page.id);
    assert_eq!(
        texts,
        vec![("paragraph".to_string(), "Overview.".to_string())]
    );
    let child_texts = page_texts(&notion, &child.id);
    assert_eq!(
        child_texts,
        vec![("paragraph".to_string(), "Content of A.".to_string())]
    );
}

#[tokio::test]
async fn index_marker_at_vault_root_appends_to_the_snapshot_root_page() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/a.org", &org("a", "Title A", "Content of A.")),
        ("/vault/b.org", &org("b", "Title B", "Content of B.")),
        ("/vault/.INDEX", "a.org\nflat = true\n"),
    ]);
    run_with(&config(), &fs, &notion).await.unwrap();

    let pages = notion.pages();
    assert_eq!(pages.len(), 1); // only the snapshot root
    let texts = page_texts(&notion, &pages[0].id);
    // The index node (a.org) leads without a heading; b.org follows
    // introduced by its title.
    let kinds: Vec<&str> = texts.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(kinds, vec!["paragraph", "heading_1", "paragraph"]);
    assert_eq!(texts[0].1, "Content of A.");
    assert_eq!(texts[1].1, "Title B");
    assert_eq!(texts[2].1, "Content of B.");
}

#[tokio::test]
async fn dry_run_marks_flat_index_directories() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/merged/a.org", &org("a", "Title A", "Body.")),
        ("/vault/merged/index.org", &org("i", "Merged Index", "Body.")),
        ("/vault/merged/.INDEX", "index.org\nflat = true\n"),
        ("/vault/normal/b.org", &org("b", "Title B", "Body.")),
    ]);
    let cfg = RunConfig {
        dry_run: true,
        ..config()
    };
    let mut reporter = CollectingReporter::default();
    execute(
        &cfg,
        &fs,
        &notion,
        &FixedClock::at("2026-07-12T14:03:00Z"),
        &FakeEnv::default(),
        &mut reporter,
    )
    .await
    .unwrap();

    let output = reporter.lines.join("\n");
    // The indexed directory shows under its index node's title.
    assert!(output.contains("  - Merged Index/ (flat)"), "got: {output}");
    assert!(output.contains("  - normal/\n"), "got: {output}");
}

#[tokio::test]
async fn dry_run_shows_planned_directory_tree() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[
        ("/vault/hub.org", &org("hub", "Hub", "Root-level node.")),
        (
            "/vault/backend/autopilot/a.org",
            &org("a", "Autopilot A", "Body."),
        ),
    ]);
    let cfg = RunConfig {
        dry_run: true,
        ..config()
    };
    let mut reporter = CollectingReporter::default();
    execute(
        &cfg,
        &fs,
        &notion,
        &FixedClock::at("2026-07-12T14:03:00Z"),
        &FakeEnv::default(),
        &mut reporter,
    )
    .await
    .unwrap();

    let output = reporter.lines.join("\n");
    assert!(output.contains("  - backend/"), "got: {output}");
    assert!(output.contains("    - autopilot/"), "got: {output}");
    assert!(output.contains("      - Autopilot A"), "got: {output}");
    assert!(output.contains("  - Hub"), "got: {output}");
    assert!(notion.pages().is_empty());
}

#[tokio::test]
async fn links_are_written_as_mentions_of_the_right_page() {
    let notion = FakeNotion::new();
    run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap();

    let pages = notion.pages();
    let page_a = &pages[1]; // sorted: a.org first
    let page_b = &pages[2];
    let content_a = serde_json::to_value(notion.children_of(&page_a.id)).unwrap();
    let mentions: Vec<_> = content_a
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|b| b["paragraph"]["rich_text"].as_array())
        .flatten()
        .filter(|rt| rt["type"] == "mention")
        .collect();
    assert_eq!(mentions.len(), 1);
    assert_eq!(mentions[0]["mention"]["page"]["id"], page_b.id.as_str());
}

#[tokio::test]
async fn default_title_uses_injected_clock() {
    let notion = FakeNotion::new();
    run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap();
    assert_eq!(
        notion.pages()[0].title,
        "Org-roam snapshot 2026-07-12T14:03:00Z"
    );
}

#[tokio::test]
async fn explicit_title_wins_over_default() {
    let notion = FakeNotion::new();
    let cfg = RunConfig {
        title: Some("My Snapshot".to_string()),
        ..config()
    };
    run_with(&cfg, &two_node_vault(), &notion).await.unwrap();
    assert_eq!(notion.pages()[0].title, "My Snapshot");
}

#[tokio::test]
async fn parent_page_id_falls_back_to_env_var() {
    let notion = FakeNotion::new();
    let cfg = RunConfig {
        parent_page_id: None,
        ..config()
    };
    let report = execute(
        &cfg,
        &two_node_vault(),
        &notion,
        &FixedClock::at("2026-07-12T14:03:00Z"),
        &FakeEnv::with(&[(PARENT_PAGE_ID_ENV_VAR, "env-parent")]),
        &mut CollectingReporter::default(),
    )
    .await;
    assert!(report.is_ok());
    assert_eq!(notion.pages()[0].parent_id, "env-parent");
}

#[tokio::test]
async fn missing_parent_page_id_fails_before_any_write() {
    let notion = FakeNotion::new();
    let cfg = RunConfig {
        parent_page_id: None,
        ..config()
    };
    let err = run_with(&cfg, &two_node_vault(), &notion)
        .await
        .unwrap_err();
    assert!(matches!(err, RunError::MissingParentPageId));
    assert!(notion.pages().is_empty());
}

#[tokio::test]
async fn broken_link_aborts_before_any_notion_call() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[(
        "/vault/a.org",
        &org("a", "Node A", "Links to [[id:ghost]]."),
    )]);
    let err = run_with(&config(), &fs, &notion).await.unwrap_err();
    match err {
        RunError::PreValidation(broken) => {
            assert_eq!(broken.len(), 1);
            assert_eq!(broken[0].target_id, "ghost");
        }
        other => panic!("expected PreValidation, got {other}"),
    }
    assert!(notion.pages().is_empty());
    assert!(notion.append_calls().is_empty());
}

#[tokio::test]
async fn dry_run_validates_and_writes_nothing() {
    let notion = FakeNotion::new();
    let cfg = RunConfig {
        dry_run: true,
        ..config()
    };
    let mut reporter = CollectingReporter::default();
    let report = execute(
        &cfg,
        &two_node_vault(),
        &notion,
        &FixedClock::at("2026-07-12T14:03:00Z"),
        &FakeEnv::default(),
        &mut reporter,
    )
    .await
    .unwrap();

    assert_eq!(report.node_count, 2);
    assert!(report.root_url.is_none());
    assert!(notion.pages().is_empty());
    assert!(notion.append_calls().is_empty());
    let output = reporter.lines.join("\n");
    assert!(
        output.contains("Node A"),
        "planned structure listed: {output}"
    );
    assert!(output.contains("Nothing was written to Notion."));
}

#[tokio::test]
async fn dry_run_still_fails_on_broken_links() {
    let notion = FakeNotion::new();
    let fs = InMemoryFileSystem::with_files(&[("/vault/a.org", &org("a", "A", "[[id:ghost]]"))]);
    let cfg = RunConfig {
        dry_run: true,
        ..config()
    };
    assert!(matches!(
        run_with(&cfg, &fs, &notion).await,
        Err(RunError::PreValidation(_))
    ));
}

#[tokio::test]
async fn large_documents_are_chunked_at_one_hundred_blocks() {
    let notion = FakeNotion::new();
    let many_paragraphs = (0..250)
        .map(|i| format!("Paragraph number {i}."))
        .collect::<Vec<_>>()
        .join("\n\n");
    let fs = InMemoryFileSystem::with_files(&[(
        "/vault/big.org",
        &org("big", "Big Node", &many_paragraphs),
    )]);
    let report = run_with(&config(), &fs, &notion).await.unwrap();

    assert_eq!(report.block_count, 250);
    let chunks: Vec<usize> = notion.append_calls().iter().map(|(_, n)| *n).collect();
    assert_eq!(chunks, vec![100, 100, 50]);
    assert!(chunks.iter().all(|&n| n <= MAX_CHILDREN_PER_REQUEST));
}

#[tokio::test]
async fn mid_run_create_failure_reports_partial_snapshot_with_root_url() {
    let mut notion = FakeNotion::new();
    notion.fail_create_after = Some(2); // root + first node page succeed
    let err = run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap_err();
    match err {
        RunError::Api { root_url, .. } => {
            assert_eq!(root_url.as_deref(), Some("https://www.notion.so/page-0"));
        }
        other => panic!("expected Api error, got {other}"),
    }
}

#[tokio::test]
async fn append_failure_reports_partial_snapshot() {
    let mut notion = FakeNotion::new();
    notion.fail_append_status = Some(500);
    let err = run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap_err();
    match err {
        RunError::Api {
            root_url, source, ..
        } => {
            assert!(root_url.is_some());
            assert!(source.to_string().contains("500"));
        }
        other => panic!("expected Api error, got {other}"),
    }
}

#[tokio::test]
async fn post_validation_failure_flags_snapshot_invalid() {
    let mut notion = FakeNotion::new();
    notion.serve_empty_children = true; // content "vanishes" on read-back
    let err = run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap_err();
    match err {
        RunError::PostValidation { root_url, failures } => {
            assert_eq!(root_url, "https://www.notion.so/page-0");
            assert_eq!(failures.len(), 1); // only node a has links
            assert_eq!(failures[0].missing, vec!["b".to_string()]);
        }
        other => panic!("expected PostValidation, got {other}"),
    }
}

#[tokio::test]
async fn auth_error_message_hints_at_token_and_sharing() {
    let mut notion = FakeNotion::new();
    notion.fail_append_status = Some(401);
    let err = run_with(&config(), &two_node_vault(), &notion)
        .await
        .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("NOTION_TOKEN"), "got: {message}");
    assert!(message.contains("shared"), "got: {message}");
}
