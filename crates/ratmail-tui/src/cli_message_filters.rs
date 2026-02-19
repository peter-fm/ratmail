use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use ratmail_core::{
    AttachmentMeta, DEFAULT_TEXT_WIDTH, Folder, MailStore, MessageSummary, SqliteMailStore,
};
use ratmail_mail::ImapConfig;

use super::{SearchSpec, extract_display, extract_email, mailaddrs_to_emails};

pub(crate) fn parse_since_ts(raw: &str) -> Result<i64> {
    mailparse::dateparse(raw).map_err(|_| anyhow::anyhow!("Invalid date for --since"))
}

pub(crate) fn parse_before_ts(raw: &str) -> Result<i64> {
    mailparse::dateparse(raw).map_err(|_| anyhow::anyhow!("Invalid date for --before"))
}

pub(crate) fn map_folder_names(folders: &[Folder]) -> HashMap<i64, String> {
    folders.iter().map(|f| (f.id, f.name.clone())).collect()
}

pub(crate) fn parse_from_addrs(input: &str) -> Vec<String> {
    match mailparse::addrparse(input) {
        Ok(list) => mailaddrs_to_emails(&list),
        Err(_) => {
            let fallback = extract_email(input);
            if fallback.is_empty() {
                Vec::new()
            } else {
                vec![fallback]
            }
        }
    }
}

pub(crate) fn from_matches_filter(from_raw: &str, filter: &str) -> bool {
    let needle = filter.to_ascii_lowercase();
    if from_raw.to_ascii_lowercase().contains(&needle) {
        return true;
    }
    for addr in parse_from_addrs(from_raw) {
        if addr.to_ascii_lowercase().contains(&needle) {
            return true;
        }
    }
    false
}

pub(crate) fn spec_matches_text_fields_cli(
    spec: &SearchSpec,
    summary: &MessageSummary,
    to: Option<&str>,
    cc: Option<&str>,
) -> bool {
    let needle = spec.text.trim();
    if !needle.is_empty()
        && !summary.from.to_ascii_lowercase().contains(needle)
        && !summary.subject.to_ascii_lowercase().contains(needle)
        && !summary.preview.to_ascii_lowercase().contains(needle)
    {
        return false;
    }
    for from in &spec.from {
        if !from_matches_filter(&summary.from, from) {
            return false;
        }
    }
    for subject in &spec.subject {
        if !summary.subject.to_ascii_lowercase().contains(subject) {
            return false;
        }
    }
    if !spec.to.is_empty() {
        let to_raw = to.unwrap_or("");
        let cc_raw = cc.unwrap_or("");
        for needle in &spec.to {
            if !to_raw.to_ascii_lowercase().contains(needle)
                && !cc_raw.to_ascii_lowercase().contains(needle)
            {
                return false;
            }
        }
    }
    for date in &spec.date {
        if !summary.date.to_ascii_lowercase().contains(date) {
            return false;
        }
    }
    if spec.since_ts.is_some() || spec.before_ts.is_some() {
        let ts = mailparse::dateparse(&summary.date).ok();
        let Some(ts) = ts else { return false };
        if let Some(since) = spec.since_ts {
            if ts < since {
                return false;
            }
        }
        if let Some(before) = spec.before_ts {
            if ts > before {
                return false;
            }
        }
    }
    true
}

pub(crate) fn spec_matches_attachments_cli(
    spec: &SearchSpec,
    attachments: &[AttachmentMeta],
) -> bool {
    for name in &spec.attachment_name {
        let matches = attachments
            .iter()
            .any(|att| att.filename.to_ascii_lowercase().contains(name));
        if !matches {
            return false;
        }
    }
    for ty in &spec.attachment_type {
        let matches = attachments.iter().any(|att| {
            let mime = att.mime.to_ascii_lowercase();
            let filename = att.filename.to_ascii_lowercase();
            if mime.contains(ty) {
                return true;
            }
            if !ty.contains('/') {
                return filename.ends_with(&format!(".{}", ty));
            }
            false
        });
        if !matches {
            return false;
        }
    }
    true
}

pub(crate) fn account_id_for(
    rt: &Arc<tokio::runtime::Runtime>,
    store: &SqliteMailStore,
    name: &str,
) -> i64 {
    rt.block_on(store.account_id_by_name(name))
        .ok()
        .flatten()
        .or_else(|| rt.block_on(store.first_account_id()).ok().flatten())
        .unwrap_or(1)
}

pub(crate) fn maybe_fetch_raw(
    rt: &Arc<tokio::runtime::Runtime>,
    store: &SqliteMailStore,
    imap: Option<&ImapConfig>,
    folder_name: Option<&str>,
    uid: Option<u32>,
    message_id: i64,
    fetch: bool,
) -> Result<Option<Vec<u8>>> {
    if let Some(raw) = rt.block_on(store.get_raw_body(message_id))? {
        return Ok(Some(raw));
    }
    if !fetch {
        return Ok(None);
    }
    let (Some(imap), Some(folder), Some(uid)) = (imap, folder_name, uid) else {
        return Ok(None);
    };
    let raw = ratmail_mail::fetch_imap_body(imap, folder, uid)?;
    rt.block_on(store.upsert_raw_body(message_id, &raw))?;
    if let Ok(display) = extract_display(&raw, DEFAULT_TEXT_WIDTH as usize) {
        let _ = rt.block_on(store.upsert_cache_text(message_id, DEFAULT_TEXT_WIDTH, &display.text));
    }
    Ok(Some(raw))
}

#[cfg(test)]
mod tests {
    use ratmail_core::AttachmentMeta;

    use super::{
        from_matches_filter, parse_before_ts, parse_since_ts, spec_matches_attachments_cli,
    };
    use crate::SearchSpec;

    #[test]
    fn parse_date_filters_accept_and_reject() {
        let since_raw = "2025-01-01";
        let before_raw = "2025-12-31";
        assert_eq!(
            parse_since_ts(since_raw).unwrap(),
            mailparse::dateparse(since_raw).unwrap()
        );
        assert_eq!(
            parse_before_ts(before_raw).unwrap(),
            mailparse::dateparse(before_raw).unwrap()
        );
    }

    #[test]
    fn from_filter_matches_name_or_email() {
        let raw = "Alice Example <alice@example.com>";
        assert!(from_matches_filter(raw, "alice"));
        assert!(from_matches_filter(raw, "example.com"));
        assert!(!from_matches_filter(raw, "bob"));
    }

    #[test]
    fn attachment_filters_match_names_and_types() {
        let mut spec = SearchSpec::default();
        spec.attachment_name.push("invoice".to_string());
        spec.attachment_type.push("pdf".to_string());
        let attachments = vec![AttachmentMeta {
            filename: "invoice-2025.pdf".to_string(),
            mime: "application/pdf".to_string(),
            size: 1024,
        }];
        assert!(spec_matches_attachments_cli(&spec, &attachments));
    }

    #[test]
    fn attachment_filters_fail_when_requirements_missing() {
        let mut spec = SearchSpec::default();
        spec.attachment_name.push("invoice".to_string());
        spec.attachment_type.push("pdf".to_string());
        let attachments = vec![AttachmentMeta {
            filename: "photo.jpg".to_string(),
            mime: "image/jpeg".to_string(),
            size: 10,
        }];
        assert!(!spec_matches_attachments_cli(&spec, &attachments));
    }
}
