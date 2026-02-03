use std::collections::HashSet;

use anyhow::{anyhow, Result};
use linkify::{LinkFinder, LinkKind};
use mailparse::ParsedMail;

#[derive(Debug, Clone)]
pub struct DisplayText {
    pub text: String,
    pub links: Vec<String>,
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
