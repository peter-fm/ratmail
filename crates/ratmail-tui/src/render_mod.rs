use std::sync::Arc;

use ratmail_content::prepare_html;
use ratmail_core::{MailStore, SqliteMailStore, TileMeta, log_debug};
use ratmail_render::{RemotePolicy, Renderer};

use super::TILE_CACHE_BUDGET_BYTES;

#[derive(Debug, Clone)]
pub(crate) struct RenderRequest {
    pub(crate) request_id: u64,
    pub(crate) message_ids: Vec<i64>,
    pub(crate) width_px: i64,
    pub(crate) tile_height_px: i64,
    pub(crate) max_tiles: Option<usize>,
    pub(crate) theme: String,
    pub(crate) remote_policy: String,
}

#[derive(Debug, Clone)]
pub(crate) struct RenderEvent {
    pub(crate) message_id: i64,
    pub(crate) tiles: Vec<TileMeta>,
    pub(crate) tile_height_px: i64,
    pub(crate) width_px: i64,
    pub(crate) no_html: bool,
    pub(crate) error: Option<String>,
}

pub(crate) async fn render_worker(
    mut rx: tokio::sync::watch::Receiver<RenderRequest>,
    tx: tokio::sync::mpsc::Sender<RenderEvent>,
    store: SqliteMailStore,
    renderer: Arc<dyn Renderer>,
) {
    loop {
        if rx.changed().await.is_err() {
            break;
        }
        let request = rx.borrow().clone();
        let current_id = request.request_id;
        let allow_remote = request.remote_policy == "allowed";
        let theme_cache_key = format!("{}:bgv2", request.theme);
        log_debug(&format!(
            "render_worker request id={} tile_h={} max_tiles={:?} msgs={:?}",
            current_id, request.tile_height_px, request.max_tiles, request.message_ids
        ));

        for message_id in request.message_ids {
            if rx.borrow().request_id != current_id {
                break;
            }

            log_debug(&format!(
                "render_worker start msg_id={} tile_h={}",
                message_id, request.tile_height_px
            ));
            let html = match store
                .get_cache_html(message_id, &request.remote_policy)
                .await
            {
                Ok(Some(html)) => Some(html),
                Ok(None) => {
                    if let Ok(Some(raw)) = store.get_raw_body(message_id).await {
                        if let Ok(Some(prepared)) = prepare_html(&raw, allow_remote) {
                            let _ = store
                                .upsert_cache_html(
                                    message_id,
                                    &request.remote_policy,
                                    &prepared.html,
                                )
                                .await;
                        }
                    }
                    store
                        .get_cache_html(message_id, &request.remote_policy)
                        .await
                        .ok()
                        .flatten()
                }
                Err(err) => {
                    if tx
                        .send(RenderEvent {
                            message_id,
                            tiles: Vec::new(),
                            tile_height_px: request.tile_height_px,
                            width_px: request.width_px,
                            no_html: false,
                            error: Some(err.to_string()),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    log_debug(&format!(
                        "render_worker html error msg_id={} err={}",
                        message_id, err
                    ));
                    continue;
                }
            };

            let Some(html) = html else {
                if tx
                    .send(RenderEvent {
                        message_id,
                        tiles: Vec::new(),
                        tile_height_px: request.tile_height_px,
                        width_px: request.width_px,
                        no_html: true,
                        error: None,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                log_debug(&format!("render_worker no_html msg_id={}", message_id));
                continue;
            };

            match store
                .get_cache_tiles(
                    message_id,
                    request.width_px,
                    request.tile_height_px,
                    &theme_cache_key,
                    &request.remote_policy,
                )
                .await
            {
                Ok(cached) if !cached.is_empty() => {
                    if tx
                        .send(RenderEvent {
                            message_id,
                            tiles: cached,
                            tile_height_px: request.tile_height_px,
                            width_px: request.width_px,
                            no_html: false,
                            error: None,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    log_debug(&format!(
                        "render_worker cache hit msg_id={} tiles={}",
                        message_id, request.tile_height_px
                    ));
                    continue;
                }
                Ok(_) => {}
                Err(err) => {
                    if tx
                        .send(RenderEvent {
                            message_id,
                            tiles: Vec::new(),
                            tile_height_px: request.tile_height_px,
                            width_px: request.width_px,
                            no_html: false,
                            error: Some(err.to_string()),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    log_debug(&format!(
                        "render_worker cache error msg_id={} err={}",
                        message_id, err
                    ));
                    continue;
                }
            }

            let render_result = renderer
                .render(ratmail_render::RenderRequest {
                    message_id,
                    width_px: request.width_px,
                    tile_height_px: request.tile_height_px,
                    max_tiles: request.max_tiles,
                    theme: &request.theme,
                    remote_policy: if allow_remote {
                        RemotePolicy::Allowed
                    } else {
                        RemotePolicy::Blocked
                    },
                    prepared_html: &html,
                })
                .await;

            match render_result {
                Ok(result) => {
                    if result.tiles.is_empty() {
                        if tx
                            .send(RenderEvent {
                                message_id,
                                tiles: Vec::new(),
                                tile_height_px: request.tile_height_px,
                                width_px: request.width_px,
                                no_html: false,
                                error: Some("Chromium produced no tiles. Try RATMAIL_CHROME_PATH=/usr/bin/chromium or RATMAIL_CHROME_NO_SANDBOX=1".to_string()),
                            })
                            .await
                            .is_err()
                        {
                            return;
                        }
                        log_debug(&format!("render_worker empty tiles msg_id={}", message_id));
                        continue;
                    }
                    let _ = store
                        .upsert_cache_tiles(
                            message_id,
                            request.width_px,
                            request.tile_height_px,
                            &theme_cache_key,
                            &request.remote_policy,
                            &result.tiles,
                        )
                        .await;
                    let _ = store.prune_cache_tiles(TILE_CACHE_BUDGET_BYTES).await;
                    if tx
                        .send(RenderEvent {
                            message_id,
                            tiles: result.tiles,
                            tile_height_px: request.tile_height_px,
                            width_px: request.width_px,
                            no_html: false,
                            error: None,
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    log_debug(&format!(
                        "render_worker rendered msg_id={} tile_h={}",
                        message_id, request.tile_height_px
                    ));
                }
                Err(err) => {
                    if tx
                        .send(RenderEvent {
                            message_id,
                            tiles: Vec::new(),
                            tile_height_px: request.tile_height_px,
                            width_px: request.width_px,
                            no_html: false,
                            error: Some(err.to_string()),
                        })
                        .await
                        .is_err()
                    {
                        return;
                    }
                    log_debug(&format!(
                        "render_worker render error msg_id={} err={}",
                        message_id, err
                    ));
                }
            }
        }
    }
}
