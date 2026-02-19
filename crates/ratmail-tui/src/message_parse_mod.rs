use std::collections::HashSet;

use mailparse::{MailAddr, MailHeaderMap, addrparse_header};

use super::MessageDetail;

pub(crate) fn build_reply(
    detail: Option<&MessageDetail>,
    raw: Option<&[u8]>,
    account_addr: &str,
    reply_all: bool,
) -> (String, String, String, String) {
    let Some(detail) = detail else {
        return (
            String::new(),
            String::new(),
            "Re:".to_string(),
            String::new(),
        );
    };
    let from_email = extract_email(&detail.from);
    let subject = if detail.subject.to_lowercase().starts_with("re:") {
        detail.subject.clone()
    } else {
        format!("Re: {}", detail.subject)
    };
    let mut body = String::new();
    body.push_str("\n\n");
    body.push_str(&format!("> On {}, {} wrote:\n", detail.date, detail.from));
    for line in detail.body.lines() {
        body.push_str("> ");
        body.push_str(line);
        body.push('\n');
    }

    let mut cc = String::new();
    if reply_all {
        if let Some(raw) = raw {
            if let Ok(parsed) = mailparse::parse_mail(raw) {
                let mut addrs = Vec::new();
                addrs.extend(extract_header_addresses(&parsed, "To"));
                addrs.extend(extract_header_addresses(&parsed, "Cc"));
                let mut seen = HashSet::new();
                let account = account_addr.trim().to_lowercase();
                let sender = from_email.trim().to_lowercase();
                let mut filtered = Vec::new();
                for addr in addrs {
                    let normalized = addr.to_lowercase();
                    if normalized.is_empty()
                        || normalized == account
                        || normalized == sender
                        || !seen.insert(normalized)
                    {
                        continue;
                    }
                    filtered.push(addr);
                }
                cc = filtered.join(", ");
            }
        }
    }

    (from_email, cc, subject, body)
}

pub(crate) fn build_forward(
    detail: Option<&MessageDetail>,
    raw: Option<&[u8]>,
) -> (String, String) {
    let Some(detail) = detail else {
        return ("Fwd:".to_string(), String::new());
    };
    let subject = if detail.subject.to_lowercase().starts_with("fwd:") {
        detail.subject.clone()
    } else {
        format!("Fwd: {}", detail.subject)
    };
    let mut original_to = String::new();
    let mut original_cc = String::new();
    if let Some(raw) = raw {
        if let Ok(parsed) = mailparse::parse_mail(raw) {
            let to_addrs = extract_header_addresses(&parsed, "To");
            if !to_addrs.is_empty() {
                original_to = to_addrs.join(", ");
            }
            let cc_addrs = extract_header_addresses(&parsed, "Cc");
            if !cc_addrs.is_empty() {
                original_cc = cc_addrs.join(", ");
            }
        }
    }
    let mut body = String::new();
    body.push_str("\n\n---------- Forwarded message ---------\n");
    body.push_str(&format!("From: {}\n", detail.from));
    if !original_to.is_empty() {
        body.push_str(&format!("To: {}\n", original_to));
    }
    if !original_cc.is_empty() {
        body.push_str(&format!("Cc: {}\n", original_cc));
    }
    body.push_str(&format!("Date: {}\n", detail.date));
    body.push_str(&format!("Subject: {}\n\n", detail.subject));
    body.push_str(&detail.body);
    (subject, body)
}

pub(crate) fn extract_header_addresses(parsed: &mailparse::ParsedMail, name: &str) -> Vec<String> {
    let Some(header) = parsed.headers.get_first_header(name) else {
        return Vec::new();
    };
    match addrparse_header(&header) {
        Ok(list) => mailaddrs_to_emails(&list),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn mailaddrs_to_emails(addrs: &[MailAddr]) -> Vec<String> {
    let mut out = Vec::new();
    for addr in addrs {
        match addr {
            MailAddr::Single(info) => {
                let email = info.addr.trim();
                if !email.is_empty() {
                    out.push(email.to_string());
                }
            }
            MailAddr::Group(group) => {
                for info in &group.addrs {
                    let email = info.addr.trim();
                    if !email.is_empty() {
                        out.push(email.to_string());
                    }
                }
            }
        }
    }
    out
}

pub(crate) fn to_from_raw(raw: &[u8]) -> Option<String> {
    let Ok(parsed) = mailparse::parse_mail(raw) else {
        return None;
    };
    let to = parsed.headers.get_first_value("To").unwrap_or_default();
    let trimmed = to.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn cc_from_raw(raw: &[u8]) -> Option<String> {
    let Ok(parsed) = mailparse::parse_mail(raw) else {
        return None;
    };
    let cc = parsed.headers.get_first_value("Cc").unwrap_or_default();
    let trimmed = cc.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

pub(crate) fn draft_headers_from_raw(raw: &[u8]) -> (String, String, String, String) {
    let Ok(parsed) = mailparse::parse_mail(raw) else {
        return (String::new(), String::new(), String::new(), String::new());
    };
    let to = parsed.headers.get_first_value("To").unwrap_or_default();
    let cc = parsed.headers.get_first_value("Cc").unwrap_or_default();
    let bcc = parsed.headers.get_first_value("Bcc").unwrap_or_default();
    let subject = parsed
        .headers
        .get_first_value("Subject")
        .unwrap_or_default();
    (to, cc, bcc, subject)
}

pub(crate) fn extract_email(input: &str) -> String {
    let trimmed = input.trim();
    if let (Some(start), Some(end)) = (trimmed.find('<'), trimmed.find('>')) {
        return trimmed[start + 1..end].trim().to_string();
    }
    trimmed.to_string()
}

#[cfg(test)]
mod tests {
    use crate::message_parse_mod::{
        cc_from_raw, draft_headers_from_raw, extract_email, to_from_raw,
    };

    #[test]
    fn extract_email_prefers_angle_addr() {
        assert_eq!(
            extract_email("Alice Example <alice@example.com>"),
            "alice@example.com"
        );
        assert_eq!(extract_email("bob@example.com"), "bob@example.com");
    }

    #[test]
    fn parse_to_and_cc_from_raw_headers() {
        let raw = b"From: sender@example.com\r\nTo: to@example.com\r\nCc: cc@example.com\r\nSubject: Test\r\n\r\nBody";
        assert_eq!(to_from_raw(raw), Some("to@example.com".to_string()));
        assert_eq!(cc_from_raw(raw), Some("cc@example.com".to_string()));
    }

    #[test]
    fn draft_headers_from_raw_reads_all_recipients() {
        let raw = b"From: sender@example.com\r\nTo: to@example.com\r\nCc: cc@example.com\r\nBcc: bcc@example.com\r\nSubject: Hello\r\n\r\nBody";
        let (to, cc, bcc, subject) = draft_headers_from_raw(raw);
        assert_eq!(to, "to@example.com");
        assert_eq!(cc, "cc@example.com");
        assert_eq!(bcc, "bcc@example.com");
        assert_eq!(subject, "Hello");
    }
}
