//! HTML rendering to image tiles (skeleton).

use anyhow::Result;

use ratmail_core::TileMeta;

#[derive(Debug, Clone, Copy)]
pub enum RemotePolicy {
    Blocked,
    Allowed,
}

#[derive(Debug, Clone)]
pub struct RenderRequest<'a> {
    pub message_id: i64,
    pub width_px: i64,
    pub theme: &'a str,
    pub remote_policy: RemotePolicy,
    pub prepared_html: &'a str,
}

#[derive(Debug, Clone)]
pub struct RenderResult {
    pub tiles: Vec<TileMeta>,
}

#[async_trait::async_trait]
pub trait Renderer: Send + Sync {
    async fn supports_images(&self) -> bool;
    async fn render(&self, request: RenderRequest<'_>) -> Result<RenderResult>;
}

#[derive(Default, Clone)]
pub struct NullRenderer;

#[async_trait::async_trait]
impl Renderer for NullRenderer {
    async fn supports_images(&self) -> bool {
        false
    }

    async fn render(&self, _request: RenderRequest<'_>) -> Result<RenderResult> {
        Ok(RenderResult { tiles: Vec::new() })
    }
}
