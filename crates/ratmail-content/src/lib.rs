use std::collections::HashSet;

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use base64::Engine;
use linkify::{LinkFinder, LinkKind};
use mailparse::{MailHeaderMap, ParsedMail};

use ratmail_core::AttachmentMeta;

#[derive(Debug, Clone)]
pub struct DisplayText {
    pub text: String,
    pub links: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct PreparedHtml {
    pub html: String,
    pub blocked_remote: usize,
}

#[derive(Debug, Clone)]
pub struct AttachmentData {
    pub filename: String,
    pub mime: String,
    pub data: Vec<u8>,
}

pub fn extract_display(raw: &[u8], width_cols: usize) -> Result<DisplayText> {
    let parsed = mailparse::parse_mail(raw)?;
    let (body, is_html) = select_body(&parsed)?;

    let (text, html_for_links) = if is_html {
        let sanitized = ammonia::clean(&body);
        let text = html2text::from_read(sanitized.as_bytes(), width_cols);
        (text, Some(sanitized))
    } else {
        (body, None)
    };

    let links = extract_links(&text, html_for_links.as_deref());

    Ok(DisplayText { text, links })
}

pub fn extract_attachments(raw: &[u8]) -> Result<Vec<AttachmentMeta>> {
    let parsed = mailparse::parse_mail(raw)?;
    let mut attachments = Vec::new();
    collect_attachments(&parsed, &mut attachments)?;
    Ok(attachments)
}

pub fn extract_attachment_data(raw: &[u8], index: usize) -> Result<Option<AttachmentData>> {
    let parsed = mailparse::parse_mail(raw)?;
    let mut current = 0usize;
    collect_attachment_at(&parsed, index, &mut current)
}

pub fn prepare_html(raw: &[u8], allow_remote: bool) -> Result<Option<PreparedHtml>> {
    let parsed = mailparse::parse_mail(raw)?;
    let html = find_html_part(&parsed)?;
    let Some(html) = html else { return Ok(None) };

    let sanitized = ammonia::clean(&html);
    let cid_map = collect_cid_map(&parsed)?;
    let mut prepared = inline_cid_images(&sanitized, &cid_map);
    let mut blocked_remote = 0;
    if !allow_remote {
        let (blocked, count) = block_remote_images(&prepared);
        prepared = blocked;
        blocked_remote = count;
    }

    Ok(Some(PreparedHtml {
        html: prepared,
        blocked_remote,
    }))
}

fn select_body(parsed: &ParsedMail) -> Result<(String, bool)> {
    if parsed.subparts.is_empty() {
        let ctype = parsed.ctype.mimetype.to_lowercase();
        let body = parsed.get_body()?;
        return Ok((body, ctype == "text/html"));
    }

    let mut text_plain: Option<String> = None;
    let mut text_html: Option<String> = None;

    walk_parts(parsed, &mut |part| {
        let ctype = part.ctype.mimetype.to_lowercase();
        if ctype == "text/plain" && text_plain.is_none() {
            if let Ok(body) = part.get_body() {
                text_plain = Some(body);
            }
        }
        if ctype == "text/html" && text_html.is_none() {
            if let Ok(body) = part.get_body() {
                text_html = Some(body);
            }
        }
    });

    if let Some(text) = text_plain {
        return Ok((text, false));
    }
    if let Some(html) = text_html {
        return Ok((html, true));
    }

    Err(anyhow!("no displayable body found"))
}

fn walk_parts<F>(parsed: &ParsedMail, cb: &mut F)
where
    F: FnMut(&ParsedMail),
{
    cb(parsed);
    for part in &parsed.subparts {
        walk_parts(part, cb);
    }
}

fn collect_attachments(parsed: &ParsedMail, out: &mut Vec<AttachmentMeta>) -> Result<()> {
    if parsed.subparts.is_empty() {
        let ctype = parsed.ctype.mimetype.to_lowercase();
        let disposition = parsed.get_content_disposition();
        let mut filename = disposition
            .params
            .get("filename")
            .cloned()
            .or_else(|| parsed.ctype.params.get("name").cloned());

        let is_attachment = matches!(
            disposition.disposition,
            mailparse::DispositionType::Attachment
        ) || filename.is_some();

        if is_attachment {
            let body = parsed.get_body_raw()?;
            let name = filename.take().unwrap_or_else(|| "attachment".to_string());
            out.push(AttachmentMeta {
                filename: name,
                mime: ctype,
                size: body.len(),
            });
        }
        return Ok(());
    }

    for part in &parsed.subparts {
        collect_attachments(part, out)?;
    }
    Ok(())
}

fn collect_attachment_at(
    parsed: &ParsedMail,
    target_index: usize,
    current: &mut usize,
) -> Result<Option<AttachmentData>> {
    if parsed.subparts.is_empty() {
        let ctype = parsed.ctype.mimetype.to_lowercase();
        let disposition = parsed.get_content_disposition();
        let mut filename = disposition
            .params
            .get("filename")
            .cloned()
            .or_else(|| parsed.ctype.params.get("name").cloned());

        let is_attachment = matches!(
            disposition.disposition,
            mailparse::DispositionType::Attachment
        ) || filename.is_some();

        if is_attachment {
            if *current == target_index {
                let body = parsed.get_body_raw()?;
                let name = filename.take().unwrap_or_else(|| "attachment".to_string());
                return Ok(Some(AttachmentData {
                    filename: name,
                    mime: ctype,
                    data: body,
                }));
            }
            *current += 1;
        }
        return Ok(None);
    }

    for part in &parsed.subparts {
        if let Some(found) = collect_attachment_at(part, target_index, current)? {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn find_html_part(parsed: &ParsedMail) -> Result<Option<String>> {
    let mut html: Option<String> = None;
    walk_parts(parsed, &mut |part| {
        if html.is_some() {
            return;
        }
        let ctype = part.ctype.mimetype.to_lowercase();
        if ctype == "text/html" {
            if let Ok(body) = part.get_body() {
                html = Some(body);
            }
        }
    });
    Ok(html)
}

fn collect_cid_map(parsed: &ParsedMail) -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    walk_parts(parsed, &mut |part| {
        let cid = part
            .headers
            .get_first_value("Content-ID")
            .map(|v| v.trim().trim_start_matches('<').trim_end_matches('>').to_string());
        if cid.is_none() {
            return;
        }
        let cid = cid.unwrap();
        let ctype = part.ctype.mimetype.to_lowercase();
        if let Ok(body) = part.get_body_raw() {
            let data = BASE64_STD.encode(body);
            let data_url = format!("data:{};base64,{}", ctype, data);
            out.push((cid, data_url));
        }
    });
    Ok(out)
}

fn inline_cid_images(html: &str, cid_map: &[(String, String)]) -> String {
    let mut out = html.to_string();
    for (cid, data_url) in cid_map {
        let needle1 = format!("src=\"cid:{}\"", cid);
        let needle2 = format!("src='cid:{}'", cid);
        out = out.replace(&needle1, &format!("src=\"{}\"", data_url));
        out = out.replace(&needle2, &format!("src='{}'", data_url));
    }
    out
}

fn block_remote_images(html: &str) -> (String, usize) {
    let mut out = html.to_string();
    let mut count = 0;
    for prefix in ["src=\"http://", "src=\"https://", "src='http://", "src='https://"] {
        let mut idx = 0;
        while let Some(pos) = out[idx..].find(prefix) {
            let start = idx + pos;
            let url_start = start + prefix.len();
            let quote = if prefix.contains('\"') { '\"' } else { '\'' };
            if let Some(end_rel) = out[url_start..].find(quote) {
                let end = url_start + end_rel;
                out.replace_range(start..end, &format!("src=\"ratmail-blocked://remote\""));
                count += 1;
                idx = start + 1;
            } else {
                break;
            }
        }
    }
    (out, count)
}

fn extract_links(text: &str, html: Option<&str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();

    if let Some(html) = html {
        for link in extract_href_links(html) {
            if seen.insert(link.clone()) {
                out.push(link);
            }
        }
    }

    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    for link in finder.links(text) {
        let url = link.as_str().to_string();
        if seen.insert(url.clone()) {
            out.push(url);
        }
    }

    out
}

fn extract_href_links(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_href = false;
    let mut quote: Option<char> = None;

    for ch in html.chars() {
        if !in_href {
            if ch == 'h' {
                buf.clear();
            }
            buf.push(ch);
            if buf.ends_with("href=") {
                in_href = true;
                buf.clear();
            }
            if buf.len() > 5 {
                buf.remove(0);
            }
            continue;
        }

        if quote.is_none() {
            if ch == '"' || ch == '\'' {
                quote = Some(ch);
                continue;
            }
            if ch.is_whitespace() {
                in_href = false;
                continue;
            }
            buf.push(ch);
            continue;
        }

        if Some(ch) == quote {
            if !buf.is_empty() {
                out.push(buf.clone());
            }
            buf.clear();
            in_href = false;
            quote = None;
            continue;
        }

        buf.push(ch);
    }

    if in_href && !buf.is_empty() {
        out.push(buf);
    }

    out
}
