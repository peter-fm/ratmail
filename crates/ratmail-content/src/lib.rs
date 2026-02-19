use std::collections::HashMap;

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STD;
use linkify::{LinkFinder, LinkKind};
use mailparse::{MailHeaderMap, ParsedMail};

use ratmail_core::{AttachmentMeta, LinkInfo};

#[derive(Debug, Clone)]
pub struct DisplayText {
    pub text: String,
    pub links: Vec<LinkInfo>,
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
        let sanitized = sanitize_html(&body);
        let text = html2text::from_read(sanitized.as_bytes(), width_cols);
        (text, Some(sanitized))
    } else if let Some(html) = find_html_part(&parsed)? {
        let sanitized = sanitize_html(&html);
        (body, Some(sanitized))
    } else {
        (body, None)
    };

    let text = html_escape::decode_html_entities(&text).to_string();
    let text = normalize_display_text(&text);
    let links = extract_links(&text, html_for_links.as_deref());
    let text = normalize_bracketed_urls(&text);
    let text = replace_link_urls_with_labels(&text, &links);

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

    let sanitized = sanitize_html(&html);
    let cid_map = collect_cid_map(&parsed)?;
    let mut prepared = inline_cid_images(&sanitized, &cid_map);
    let mut blocked_remote = 0;
    if !allow_remote {
        let (blocked, count) = block_remote_assets(&prepared);
        prepared = blocked;
        blocked_remote = count;
    }

    Ok(Some(PreparedHtml {
        html: prepared,
        blocked_remote,
    }))
}

fn sanitize_html(html: &str) -> String {
    let mut builder = ammonia::Builder::default();
    builder.rm_clean_content_tags(["style"]);
    builder.add_tags(["style", "font"]);
    builder.add_generic_attributes(["style", "background", "bgcolor"]);
    builder.add_tag_attributes("font", ["face", "size", "color"]);
    builder.add_tag_attributes("table", ["background", "bgcolor"]);
    builder.add_tag_attributes("td", ["background", "bgcolor"]);
    builder.add_tag_attributes("body", ["background", "bgcolor"]);
    builder.clean(html).to_string()
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
        let cid = part.headers.get_first_value("Content-ID").map(|v| {
            v.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        });
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

fn block_remote_assets(html: &str) -> (String, usize) {
    let mut out = html.to_string();
    let mut count = 0;
    for prefix in [
        "src=\"http://",
        "src=\"https://",
        "src='http://",
        "src='https://",
        "background=\"http://",
        "background=\"https://",
        "background='http://",
        "background='https://",
    ] {
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
    let (out, css_count) = block_remote_css_urls(&out);
    (out, count + css_count)
}

fn block_remote_css_urls(html: &str) -> (String, usize) {
    let mut out = String::with_capacity(html.len());
    let mut count = 0;
    let mut idx = 0;
    let bytes = html.as_bytes();

    while let Some(pos) = html[idx..].find("url(") {
        let start = idx + pos;
        out.push_str(&html[idx..start]);
        let mut j = start + 4;

        while j < html.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }

        let mut quote: Option<char> = None;
        if j < html.len() {
            let b = bytes[j];
            if b == b'\'' || b == b'"' {
                quote = Some(b as char);
                j += 1;
            }
        }

        let url_start = j;
        let end = if let Some(q) = quote {
            match html[url_start..].find(q) {
                Some(rel) => url_start + rel,
                None => {
                    out.push_str(&html[start..]);
                    return (out, count);
                }
            }
        } else {
            match html[url_start..].find(')') {
                Some(rel) => url_start + rel,
                None => {
                    out.push_str(&html[start..]);
                    return (out, count);
                }
            }
        };

        let url = html[url_start..end].trim_start();
        let is_remote = url.starts_with("http://") || url.starts_with("https://");

        let mut end_paren = end;
        if quote.is_some() {
            end_paren += 1;
            while end_paren < html.len() && bytes[end_paren].is_ascii_whitespace() {
                end_paren += 1;
            }
            if end_paren < html.len() && bytes[end_paren] == b')' {
                end_paren += 1;
            } else {
                out.push_str(&html[start..end_paren]);
                idx = end_paren;
                continue;
            }
        } else {
            end_paren = end + 1;
        }

        if is_remote {
            count += 1;
            out.push_str("url(\"ratmail-blocked://remote\")");
        } else {
            out.push_str(&html[start..end_paren]);
        }

        idx = end_paren;
    }

    out.push_str(&html[idx..]);
    (out, count)
}

fn extract_links(text: &str, html: Option<&str>) -> Vec<LinkInfo> {
    let mut out: Vec<LinkInfo> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();

    if let Some(html) = html {
        for (url, text) in extract_href_links_with_text(html) {
            let normalized = normalize_link_text(&text);
            if let Some(idx) = seen.get(&url).copied() {
                if out[idx].text.is_none() && normalized.is_some() {
                    out[idx].text = normalized;
                }
                continue;
            }
            out.push(LinkInfo {
                url: url.clone(),
                text: normalized,
                from_html: true,
            });
            seen.insert(url, out.len() - 1);
        }
    }

    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]);
    for link in finder.links(text) {
        let url = link.as_str().to_string();
        if seen.contains_key(&url) {
            continue;
        }
        out.push(LinkInfo {
            url: url.clone(),
            text: None,
            from_html: false,
        });
        seen.insert(url, out.len() - 1);
    }

    out
}

fn extract_href_links_with_text(html: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let lower = html.to_ascii_lowercase();
    let mut idx = 0usize;

    while let Some(pos) = lower[idx..].find("<a") {
        let start = idx + pos;
        let tag_end = match lower[start..].find('>') {
            Some(rel) => start + rel,
            None => break,
        };
        let tag = &lower[start..=tag_end];
        let href_pos = match tag.find("href=") {
            Some(pos) => start + pos + 5,
            None => {
                idx = tag_end + 1;
                continue;
            }
        };

        let mut j = href_pos;
        let bytes = html.as_bytes();
        while j < html.len() && bytes[j].is_ascii_whitespace() {
            j += 1;
        }
        let mut quote: Option<char> = None;
        if j < html.len() && (bytes[j] == b'\'' || bytes[j] == b'"') {
            quote = Some(bytes[j] as char);
            j += 1;
        }
        let url_start = j;
        let url_end = if let Some(q) = quote {
            match html[url_start..].find(q) {
                Some(rel) => url_start + rel,
                None => {
                    idx = tag_end + 1;
                    continue;
                }
            }
        } else {
            match html[url_start..].find(|c: char| c.is_whitespace() || c == '>') {
                Some(rel) => url_start + rel,
                None => {
                    idx = tag_end + 1;
                    continue;
                }
            }
        };
        let url = html[url_start..url_end].trim().to_string();
        let tag_original = &html[start..=tag_end];

        let close_tag = match lower[tag_end + 1..].find("</a") {
            Some(rel) => tag_end + 1 + rel,
            None => {
                out.push((url, String::new()));
                idx = tag_end + 1;
                continue;
            }
        };
        let inner = html[tag_end + 1..close_tag].to_string();
        let text = best_link_text(tag_original, &inner);
        out.push((url, text));
        idx = close_tag + 4;
    }

    out
}

fn normalize_link_text(text: &str) -> Option<String> {
    let stripped = strip_html_tags(text);
    let mut out = String::new();
    for part in stripped.split_whitespace() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(part);
    }
    if out.is_empty() { None } else { Some(out) }
}

fn best_link_text(tag: &str, inner: &str) -> String {
    if let Some(text) = normalize_link_text(inner) {
        return text;
    }
    if let Some(text) =
        extract_attr_value(tag, "aria-label").or_else(|| extract_attr_value(tag, "title"))
    {
        return text;
    }
    if let Some(text) =
        extract_attr_value(inner, "alt").or_else(|| extract_attr_value(inner, "title"))
    {
        return text;
    }
    String::new()
}

fn extract_attr_value(input: &str, name: &str) -> Option<String> {
    let lower = input.to_ascii_lowercase();
    let needle = format!("{}=", name);
    let pos = lower.find(&needle)?;
    let mut i = pos + needle.len();
    while i < input.len() && input.as_bytes()[i].is_ascii_whitespace() {
        i += 1;
    }
    if i >= input.len() {
        return None;
    }
    let quote = input.as_bytes()[i];
    if quote != b'\'' && quote != b'\"' {
        return None;
    }
    i += 1;
    let start = i;
    while i < input.len() && input.as_bytes()[i] != quote {
        i += 1;
    }
    if i <= start {
        return None;
    }
    let value = input[start..i].trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

fn strip_html_tags(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    for ch in input.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ => {
                if !in_tag {
                    out.push(ch);
                }
            }
        }
    }
    out
}

fn link_label_for_text(links: &[LinkInfo], idx: usize) -> Option<String> {
    let link = links.get(idx)?;
    if let Some(text) = link.text.as_deref() {
        return Some(text.to_string());
    }
    if link.from_html {
        return Some(format!("Image Link {}", idx + 1));
    }
    None
}

fn normalize_bracketed_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '[' {
            out.push(ch);
            continue;
        }
        let mut buf = String::new();
        let mut found_end = false;
        while let Some(next) = chars.next() {
            if next == ']' {
                found_end = true;
                break;
            }
            if next != '\n' && next != '\r' {
                buf.push(next);
            }
        }
        if found_end {
            let trimmed = buf.trim();
            if trimmed.starts_with("http://")
                || trimmed.starts_with("https://")
                || trimmed.starts_with("mailto:")
            {
                out.push('[');
                out.push_str(trimmed);
                out.push(']');
            } else {
                out.push('[');
                out.push_str(&buf);
                out.push(']');
            }
        } else {
            out.push('[');
            out.push_str(&buf);
        }
    }
    out
}

fn replace_link_urls_with_labels(text: &str, links: &[LinkInfo]) -> String {
    if links.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    for (idx, link) in links.iter().enumerate() {
        let Some(label) = link_label_for_text(links, idx) else {
            continue;
        };
        let label_bracketed = format!("[{}]", label);
        let bracketed = format!("[{}]", link.url);
        let token = format!("{} {}", label, bracketed);
        if out.contains(&token) {
            out = out.replace(&token, &label_bracketed);
            continue;
        }
        let token = format!("{}\n{}", label, bracketed);
        if out.contains(&token) {
            out = out.replace(&token, &label_bracketed);
            continue;
        }
        if out.contains(&bracketed) {
            out = out.replace(&bracketed, &label_bracketed);
        }
    }
    out
}

fn normalize_bracketed_labels(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '[' {
            out.push(ch);
            continue;
        }
        let mut buf = String::new();
        let mut found_end = false;
        while let Some(next) = chars.next() {
            if next == ']' {
                found_end = true;
                break;
            }
            buf.push(next);
        }
        if found_end {
            let trimmed = buf.trim();
            out.push('[');
            out.push_str(trimmed);
            out.push(']');
        } else {
            out.push('[');
            out.push_str(&buf);
        }
    }
    out
}

fn normalize_display_text(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }

    let mut out_lines: Vec<String> = Vec::new();
    let mut prev_blank = false;
    for line in normalized.split('\n') {
        let trimmed = line.trim();
        if is_horizontal_rule(trimmed) {
            continue;
        }
        let is_blank = trimmed.is_empty();
        if is_blank {
            if prev_blank {
                continue;
            }
            prev_blank = true;
            out_lines.push(String::new());
        } else {
            prev_blank = false;
            out_lines.push(line.to_string());
        }
    }

    normalize_bracketed_labels(&out_lines.join("\n"))
}

fn is_horizontal_rule(trimmed: &str) -> bool {
    let mut seen: Option<char> = None;
    let mut count = 0usize;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if !is_rule_char(ch) {
            return false;
        }
        count += 1;
        seen = Some(seen.unwrap_or(ch));
    }
    count >= 3 && seen.is_some()
}

fn is_rule_char(ch: char) -> bool {
    matches!(
        ch,
        '-' | '_' | '=' | '*' | '~' | '—' | '–' | '─' | '━' | '·' | '•'
    )
}
