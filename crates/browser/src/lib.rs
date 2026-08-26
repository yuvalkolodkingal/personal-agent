//! Replaceable browser engine with isolated profiles and invalidatable handles.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

/// Opaque node handle valid only for one page generation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeHandle {
    pub page_id: String,
    pub generation: u64,
    pub opaque_id: String,
}

/// Structured page representation preferred over pixels.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PageSnapshot {
    pub page_id: String,
    pub generation: u64,
    pub url: Url,
    pub title: String,
    pub text: String,
    pub handles: Vec<NodeHandle>,
}

/// Browser operation failure.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum BrowserError {
    #[error("domain is blocked by policy: {0}")]
    DomainBlocked(String),
    #[error("page handle is stale and must be reacquired")]
    StaleHandle,
    #[error("browser capability unavailable: {0}")]
    Unavailable(String),
    #[error("browser operation failed: {0}")]
    Operation(String),
}

/// CDP is an implementation detail behind this engine boundary.
#[async_trait]
pub trait BrowserEngine: Send {
    async fn open_isolated_profile(&mut self, profile_id: &str) -> Result<(), BrowserError>;
    async fn navigate(&mut self, url: &Url) -> Result<PageSnapshot, BrowserError>;
    async fn snapshot(&mut self) -> Result<PageSnapshot, BrowserError>;
    async fn click(&mut self, handle: &NodeHandle) -> Result<PageSnapshot, BrowserError>;
    async fn type_text(
        &mut self,
        handle: &NodeHandle,
        text: &str,
    ) -> Result<PageSnapshot, BrowserError>;
    async fn takeover(&mut self) -> Result<(), BrowserError>;
    async fn close(&mut self) -> Result<(), BrowserError>;
}

/// Reject a handle after navigation or any page-changing action.
///
/// # Errors
///
/// Returns `StaleHandle` when page identity or generation differs.
pub fn validate_handle(snapshot: &PageSnapshot, handle: &NodeHandle) -> Result<(), BrowserError> {
    if handle.page_id == snapshot.page_id && handle.generation == snapshot.generation {
        Ok(())
    } else {
        Err(BrowserError::StaleHandle)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn changing_generation_invalidates_handles() {
        let handle = NodeHandle {
            page_id: "p".into(),
            generation: 1,
            opaque_id: "n1".into(),
        };
        let page = PageSnapshot {
            page_id: "p".into(),
            generation: 2,
            url: Url::parse("https://example.com").unwrap(),
            title: String::new(),
            text: String::new(),
            handles: vec![],
        };
        assert_eq!(
            validate_handle(&page, &handle),
            Err(BrowserError::StaleHandle)
        );
    }
}
