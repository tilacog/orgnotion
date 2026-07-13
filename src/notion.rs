//! Transport-independent Notion helpers: chunking and pagination logic
//! written against the [`NotionApi`] port, so both are unit-testable with
//! a fake implementation.

use crate::ports::{ChildBlock, NotionApi, NotionError};
use notionrs_types::object::block::Block;
use std::collections::VecDeque;

/// Pinned Notion API version, sent as the `Notion-Version` header on
/// every request. This must match the version the `notionrs` crate pins
/// internally (it does, as of notionrs 0.28.0) — the adapter's own
/// requests send this header, and the typed block shapes assume it.
/// Bump deliberately alongside a notionrs upgrade, not independently.
pub const NOTION_VERSION: &str = "2026-03-11";

/// Notion's hard limit on children per `PATCH .../children` call.
pub const MAX_CHILDREN_PER_REQUEST: usize = 100;

/// Append `children` to `block_id` in chunks of at most
/// [`MAX_CHILDREN_PER_REQUEST`], in order.
///
/// # Errors
///
/// Propagates the first API failure; earlier chunks may already have been
/// written (the caller reports the snapshot as incomplete in that case).
pub async fn append_children_chunked(
    notion: &impl NotionApi,
    block_id: &str,
    children: &[Block],
) -> Result<(), NotionError> {
    for chunk in children.chunks(MAX_CHILDREN_PER_REQUEST) {
        notion.append_children(block_id, chunk).await?;
    }
    Ok(())
}

/// Fetch all direct child blocks of `block_id`, following pagination
/// cursors until exhausted.
///
/// # Errors
///
/// Propagates any API failure.
pub async fn fetch_all_children(
    notion: &impl NotionApi,
    block_id: &str,
) -> Result<Vec<ChildBlock>, NotionError> {
    let mut all = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = notion.list_children(block_id, cursor.as_deref()).await?;
        all.extend(page.blocks);
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(all)
}

/// Fetch every block nested under `block_id`, flattened into a single
/// depth-first list — so post-validation finds mention rich-text even
/// when Notion nests it inside list items or toggles.
///
/// Iterative (explicit stack) rather than recursive: a recursive
/// `async fn` would need boxed futures.
///
/// # Errors
///
/// Propagates any API failure.
pub async fn fetch_all_children_recursive(
    notion: &impl NotionApi,
    block_id: &str,
) -> Result<Vec<ChildBlock>, NotionError> {
    let mut all = Vec::new();
    // Depth-first with an explicit stack of unprocessed-sibling queues (a
    // recursive `async fn` would need boxed futures): a block's nested
    // children are emitted before its next sibling, exactly like the
    // recursive formulation.
    let mut stack = vec![VecDeque::from(fetch_all_children(notion, block_id).await?)];
    while let Some(siblings) = stack.last_mut() {
        let Some(child) = siblings.pop_front() else {
            stack.pop();
            continue;
        };
        let has_children = child.has_children;
        let child_id = child.id.clone();
        all.push(child);
        if has_children {
            stack.push(VecDeque::from(fetch_all_children(notion, &child_id).await?));
        }
    }
    Ok(all)
}
