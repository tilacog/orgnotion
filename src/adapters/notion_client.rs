//! [`NotionApi`] implementation backed by the `notionrs` crate (async
//! reqwest transport). Adds retry with exponential backoff on 429 and
//! transient 5xx, which notionrs does not provide.
//!
//! `list_children` deliberately bypasses notionrs's `get_block_children`:
//! in notionrs 0.28.0 that endpoint discards the response's real
//! `has_more`/`next_cursor` (echoing the request's cursor back instead —
//! a leftover of a removed internal fetch-all loop), which breaks
//! cursor-following for pages with more than 100 blocks. We issue that
//! one GET ourselves, with the same pinned API version, and deserialize
//! into the same `notionrs_types` block model.

use crate::notion::NOTION_VERSION;
use crate::ports::{ChildBlock, ChildrenPage, CreatedPage, NotionApi, NotionError};
use notionrs_types::object::block::Block;
use notionrs_types::object::page::{PageProperty, title::PageTitleProperty};
use serde::Deserialize;
use std::collections::HashMap;
use std::time::Duration;
use tokio::time::sleep;

const API_BASE: &str = "https://api.notion.com/v1";
const MAX_ATTEMPTS: u32 = 5;
const BACKOFF_BASE_MS: u64 = 300;

/// Real Notion client, wrapping [`notionrs::Client`].
pub struct NotionrsApi {
    client: notionrs::Client,
    /// Bare HTTP client for the `list_children` workaround (see module
    /// docs); carries the same auth and version headers.
    http: reqwest::Client,
    token: String,
    /// Base URL for the workaround GET only — notionrs pins its own URLs.
    /// Overridable so tests can point `list_children` at a local stub.
    base_url: String,
    backoff_base_ms: u64,
}

impl NotionrsApi {
    /// Build a client using `token` as the integration bearer token.
    #[must_use]
    pub fn new(token: String) -> Self {
        Self::with_base_url(token, API_BASE.to_string())
    }

    /// Like [`Self::new`] but with a different base URL for the
    /// `list_children` GET — lets the test suite exercise that path
    /// against a local stub server.
    #[must_use]
    pub fn with_base_url(token: String, base_url: String) -> Self {
        Self {
            client: notionrs::Client::new(&token),
            http: reqwest::Client::new(),
            token,
            base_url,
            backoff_base_ms: BACKOFF_BASE_MS,
        }
    }

    /// Shrink retry backoff (test-only knob to keep retry tests fast).
    #[doc(hidden)]
    pub fn set_backoff_base_ms(&mut self, ms: u64) {
        self.backoff_base_ms = ms;
    }

    /// Run a notionrs call, retrying 429s, 5xx and transport failures
    /// with exponential backoff. `call` builds a fresh request future per
    /// attempt.
    ///
    /// notionrs does not surface the `Retry-After` header, so 429s are
    /// backed off exponentially like any other retryable failure.
    async fn with_retry<T, F, Fut>(&self, mut call: F) -> Result<T, NotionError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, notionrs::error::Error>>,
    {
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            match call().await {
                Ok(value) => return Ok(value),
                Err(e) => {
                    if is_retryable(&e) && attempt < MAX_ATTEMPTS {
                        sleep(retry_delay(self.backoff_base_ms, attempt, None)).await;
                        continue;
                    }
                    return Err(map_error(e));
                }
            }
        }
    }
}

fn is_retryable(error: &notionrs::error::Error) -> bool {
    match error {
        notionrs::error::Error::Http { status, .. } => {
            *status == 429 || (500..600).contains(status)
        }
        notionrs::error::Error::Network(_) => true,
        _ => false,
    }
}

fn map_error(error: notionrs::error::Error) -> NotionError {
    match error {
        notionrs::error::Error::Http { status, message } => NotionError::Api {
            status,
            body: message,
        },
        notionrs::error::Error::Network(message) => NotionError::Transport(message),
        notionrs::error::Error::SerdeJson(e) => NotionError::UnexpectedShape(e.to_string()),
        other => NotionError::Transport(other.to_string()),
    }
}

fn retry_delay(base_ms: u64, attempt: u32, retry_after_secs: Option<u64>) -> Duration {
    retry_after_secs.map_or_else(
        || Duration::from_millis(base_ms * 2u64.pow(attempt.saturating_sub(1))),
        Duration::from_secs,
    )
}

/// The slice of Notion's list-children response this tool needs; the
/// block content itself deserializes through the typed
/// [`notionrs_types`] model.
#[derive(Deserialize)]
struct ChildrenListDto {
    results: Vec<ChildDto>,
    #[serde(default)]
    has_more: bool,
    next_cursor: Option<String>,
}

#[derive(Deserialize)]
struct ChildDto {
    id: String,
    #[serde(default)]
    has_children: bool,
    #[serde(flatten)]
    block: Block,
}

impl NotionApi for NotionrsApi {
    async fn create_page(
        &self,
        parent_page_id: &str,
        title: &str,
    ) -> Result<CreatedPage, NotionError> {
        let page = self
            .with_retry(|| async {
                let mut properties = HashMap::new();
                properties.insert(
                    "title".to_string(),
                    PageProperty::Title(PageTitleProperty::from(title)),
                );
                self.client
                    .create_page::<HashMap<String, PageProperty>>()
                    .page_id(parent_page_id)
                    .properties(properties)
                    .send()
                    .await?
                    .into_page()
            })
            .await?;
        Ok(CreatedPage {
            id: page.id,
            url: page.url,
        })
    }

    async fn append_children(&self, block_id: &str, children: &[Block]) -> Result<(), NotionError> {
        self.with_retry(|| {
            self.client
                .append_block_children()
                .block_id(block_id)
                .children(children.to_vec())
                .send()
        })
        .await?;
        Ok(())
    }

    async fn list_children(
        &self,
        block_id: &str,
        cursor: Option<&str>,
    ) -> Result<ChildrenPage, NotionError> {
        let base = format!("{}/blocks/{block_id}/children?page_size=100", self.base_url);
        let url = match cursor {
            Some(c) => format!("{base}&start_cursor={c}"),
            None => base,
        };

        let mut attempt = 0u32;
        let body = loop {
            attempt += 1;
            let sent = self
                .http
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.token))
                .header("Notion-Version", NOTION_VERSION)
                .send()
                .await;
            match sent {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = retry_after_seconds(&resp);
                    let text = resp
                        .text()
                        .await
                        .map_err(|e| NotionError::Transport(e.to_string()))?;
                    if (200..300).contains(&status) {
                        break text;
                    }
                    let retryable = status == 429 || (500..600).contains(&status);
                    if retryable && attempt < MAX_ATTEMPTS {
                        sleep(retry_delay(self.backoff_base_ms, attempt, retry_after)).await;
                        continue;
                    }
                    return Err(NotionError::Api { status, body: text });
                }
                Err(e) => {
                    if attempt < MAX_ATTEMPTS {
                        sleep(retry_delay(self.backoff_base_ms, attempt, None)).await;
                        continue;
                    }
                    return Err(NotionError::Transport(e.to_string()));
                }
            }
        };

        let dto: ChildrenListDto =
            serde_json::from_str(&body).map_err(|e| NotionError::UnexpectedShape(e.to_string()))?;
        let next_cursor = if dto.has_more { dto.next_cursor } else { None };
        Ok(ChildrenPage {
            blocks: dto
                .results
                .into_iter()
                .map(|c| ChildBlock {
                    id: c.id,
                    has_children: c.has_children,
                    block: c.block,
                })
                .collect(),
            next_cursor,
        })
    }
}

fn retry_after_seconds(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

// Unit tests live in-module (not tests/) because retry and error mapping
// are private, and the notionrs-backed calls they wrap pin
// api.notion.com — they cannot be pointed at a stub server the way
// `list_children` can.
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn api() -> NotionrsApi {
        let mut api = NotionrsApi::with_base_url("t".to_string(), "http://unused".to_string());
        api.set_backoff_base_ms(0);
        api
    }

    fn http_error(status: u16) -> notionrs::error::Error {
        notionrs::error::Error::Http {
            status,
            message: format!("status {status}"),
        }
    }

    #[tokio::test]
    async fn with_retry_returns_first_success() {
        let calls = Cell::new(0u32);
        let result = api()
            .with_retry(|| {
                calls.set(calls.get() + 1);
                async { Ok(7) }
            })
            .await;
        assert_eq!(result.unwrap(), 7);
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn with_retry_retries_retryable_errors_until_success() {
        let calls = Cell::new(0u32);
        let result = api()
            .with_retry(|| {
                calls.set(calls.get() + 1);
                let outcome = if calls.get() < 3 {
                    Err(http_error(503))
                } else {
                    Ok("done")
                };
                async { outcome }
            })
            .await;
        assert_eq!(result.unwrap(), "done");
        assert_eq!(calls.get(), 3);
    }

    #[tokio::test]
    async fn with_retry_fails_fast_on_non_retryable_error() {
        let calls = Cell::new(0u32);
        let result: Result<(), _> = api()
            .with_retry(|| {
                calls.set(calls.get() + 1);
                async { Err(http_error(404)) }
            })
            .await;
        assert!(matches!(result, Err(NotionError::Api { status: 404, .. })));
        assert_eq!(calls.get(), 1);
    }

    #[tokio::test]
    async fn with_retry_gives_up_after_max_attempts() {
        let calls = Cell::new(0u32);
        let result: Result<(), _> = api()
            .with_retry(|| {
                calls.set(calls.get() + 1);
                async {
                    Err(notionrs::error::Error::Network(
                        "connection reset".to_string(),
                    ))
                }
            })
            .await;
        assert!(matches!(result, Err(NotionError::Transport(_))));
        assert_eq!(calls.get(), MAX_ATTEMPTS);
    }

    #[test]
    fn retryable_statuses_are_429_and_5xx_and_network() {
        assert!(is_retryable(&http_error(429)));
        assert!(is_retryable(&http_error(500)));
        assert!(is_retryable(&http_error(599)));
        assert!(is_retryable(
            &notionrs::error::Error::Network(String::new())
        ));
        assert!(!is_retryable(&http_error(400)));
        assert!(!is_retryable(&notionrs::error::Error::RequestParameter(
            String::new()
        )));
    }

    #[test]
    fn errors_map_onto_the_port_error_kinds() {
        assert!(matches!(
            map_error(http_error(401)),
            NotionError::Api { status: 401, .. }
        ));
        assert!(matches!(
            map_error(notionrs::error::Error::Network("down".to_string())),
            NotionError::Transport(m) if m == "down"
        ));
        let serde_err = serde_json::from_str::<u8>("not json").unwrap_err();
        assert!(matches!(
            map_error(notionrs::error::Error::SerdeJson(serde_err)),
            NotionError::UnexpectedShape(_)
        ));
        assert!(matches!(
            map_error(notionrs::error::Error::RequestParameter("p".to_string())),
            NotionError::Transport(_)
        ));
    }
}
