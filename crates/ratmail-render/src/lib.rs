//! HTML rendering to image tiles (skeleton).

use anyhow::Result;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use headless_chrome::{Browser, LaunchOptionsBuilder};
use image::{DynamicImage, GenericImageView, ImageBuffer, ImageFormat, Rgba, imageops::crop_imm};
use std::io::Cursor;
use std::path::PathBuf;
use std::time::{Duration, Instant};

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
    pub tile_height_px: i64,
    pub max_tiles: Option<usize>,
    pub theme: &'a str,
    pub remote_policy: RemotePolicy,
    pub prepared_html: &'a str,
}

#[derive(Debug, Clone)]
pub struct RenderResult {
    pub tiles: Vec<TileMeta>,
}

pub fn detect_image_support() -> bool {
    if std::env::var("RATMAIL_FORCE_IMAGES")
        .ok()
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        return true;
    }
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    let kitty = std::env::var("KITTY_WINDOW_ID").is_ok();
    let ghostty = term_program.contains("ghostty") || term.contains("ghostty");
    let iterm = term_program.contains("iterm");
    let sixel = term.contains("sixel");
    kitty || iterm || sixel || ghostty
}

#[async_trait::async_trait]
pub trait Renderer: Send + Sync {
    async fn supports_images(&self) -> bool;
    async fn render(&self, request: RenderRequest<'_>) -> Result<RenderResult>;
}

#[derive(Default, Clone)]
pub struct NullRenderer;

#[derive(Default, Clone)]
pub struct ChromiumRenderer;

#[async_trait::async_trait]
impl Renderer for NullRenderer {
    async fn supports_images(&self) -> bool {
        false
    }

    async fn render(&self, request: RenderRequest<'_>) -> Result<RenderResult> {
        let width = request.width_px.max(1) as u32;
        let height = 240u32;
        let mut img = ImageBuffer::from_pixel(width, height, Rgba([20, 22, 24, 255]));

        for y in (0..height).step_by(24) {
            for x in 0..width {
                img.put_pixel(x, y, Rgba([60, 65, 70, 255]));
            }
        }

        let mut png_bytes = Vec::new();
        let dyn_img = image::DynamicImage::ImageRgba8(img);
        dyn_img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png)?;

        Ok(RenderResult {
            tiles: vec![TileMeta {
                tile_index: 0,
                height_px: height as i64,
                bytes: png_bytes,
            }],
        })
    }
}

#[async_trait::async_trait]
impl Renderer for ChromiumRenderer {
    async fn supports_images(&self) -> bool {
        true
    }

    async fn render(&self, request: RenderRequest<'_>) -> Result<RenderResult> {
        let html = request.prepared_html.to_string();
        let width_px = request.width_px.max(300) as u32;
        let tiles = tokio::task::spawn_blocking(move || -> Result<Vec<TileMeta>> {
            let chrome_path = std::env::var("RATMAIL_CHROME_PATH")
                .ok()
                .or_else(|| std::env::var("CHROME_PATH").ok())
                .map(PathBuf::from)
                .or_else(|| {
                    let fallback = PathBuf::from("/usr/bin/chromium");
                    if fallback.exists() {
                        Some(fallback)
                    } else {
                        None
                    }
                });
            let no_sandbox = std::env::var("RATMAIL_CHROME_NO_SANDBOX")
                .ok()
                .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
                .unwrap_or(false);
            let mut builder = LaunchOptionsBuilder::default();
            builder.headless(true);
            builder.window_size(Some((width_px, 900)));
            if no_sandbox {
                builder.sandbox(false);
            }
            if let Some(path) = chrome_path {
                builder.path(Some(path));
            }
            let options = builder.build().map_err(|e| anyhow::anyhow!(e))?;
            let browser = Browser::new(options)?;
            let tab = browser.new_tab()?;
            let wrapped_html = format!(
                "<!doctype html><html><head><meta charset=\"utf-8\">\
<style>html,body{{margin:0; padding:0; width:{}px; overflow:hidden; background:#ffffff; color:#111111;}}\
*{{box-sizing:border-box;}}\
body > table, body > div{{margin-left:auto; margin-right:auto;}}\
img{{max-width:100%; height:auto;}}\
</style></head><body>{}</body></html>",
                width_px, html
            );
            let data_url = format!(
                "data:text/html;base64,{}",
                BASE64_STD.encode(wrapped_html.as_bytes())
            );
            tab.navigate_to(&data_url)?;
            // Data URLs sometimes don't emit the navigation event expected by wait_until_navigated.
            // Waiting for the body element is sufficient for our static HTML rendering.
            tab.wait_for_element("body")?;
            // Give images a moment to load so the screenshot captures them.
            // This is especially important for remote images and slower connections.
            let wait_ms = std::env::var("RATMAIL_RENDER_WAIT_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(750);
            if wait_ms > 0 {
                let deadline = Instant::now() + Duration::from_millis(wait_ms);
                loop {
                    let loaded = tab
                        .evaluate(
                            "Array.from(document.images).every(img => img.complete)",
                            false,
                        )?
                        .value
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if loaded || Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            let png = {
                use headless_chrome::protocol::cdp::Page;
                let data = tab
                    .call_method(Page::CaptureScreenshot {
                        format: Some(Page::CaptureScreenshotFormatOption::Png),
                        quality: None,
                        clip: None,
                        from_surface: Some(true),
                        capture_beyond_viewport: Some(true),
                        optimize_for_speed: None,
                    })?
                    .data;
                base64::prelude::BASE64_STANDARD.decode(data)?
            };

            let img = image::load_from_memory(&png)?;
            let tiles = slice_image_into_tiles(
                img,
                request.tile_height_px as u32,
                request.max_tiles,
                request.message_id,
            );
            if tiles.is_empty() {
                return Err(anyhow::anyhow!(
                    "Chromium produced no tiles (try RATMAIL_CHROME_PATH=/usr/bin/chromium or RATMAIL_CHROME_NO_SANDBOX=1)"
                ));
            }
            Ok(tiles)
        })
        .await??;

        Ok(RenderResult { tiles })
    }
}

fn slice_image_into_tiles(
    img: DynamicImage,
    tile_height: u32,
    max_tiles: Option<usize>,
    _message_id: i64,
) -> Vec<TileMeta> {
    let (width, height) = img.dimensions();
    let mut tiles = Vec::new();
    let mut y = 0;
    let mut index = 0;

    while y < height {
        if let Some(limit) = max_tiles {
            if tiles.len() >= limit {
                break;
            }
        }
        let h = tile_height.min(height - y);
        let cropped = crop_imm(&img, 0, y, width, h).to_image();
        let mut png_bytes = Vec::new();
        let dyn_img = image::DynamicImage::ImageRgba8(cropped);
        let _ = dyn_img.write_to(&mut Cursor::new(&mut png_bytes), ImageFormat::Png);
        tiles.push(TileMeta {
            tile_index: index,
            height_px: h as i64,
            bytes: png_bytes,
        });
        y += h;
        index += 1;
    }

    tiles
}
