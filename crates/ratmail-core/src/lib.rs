use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: i64,
    pub name: String,
    pub address: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Folder {
    pub id: i64,
    pub account_id: i64,
    pub name: String,
    pub unread: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSummary {
    pub id: i64,
    pub folder_id: i64,
    pub time: String,
    pub from: String,
    pub subject: String,
    pub unread: bool,
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageViewMeta {
    pub subject: String,
    pub from: String,
    pub date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FakeStore {
    pub account: Account,
    pub folders: Vec<Folder>,
    pub messages: Vec<MessageSummary>,
    pub message_meta: MessageViewMeta,
}

impl FakeStore {
    pub fn demo() -> Self {
        let account = Account {
            id: 1,
            name: "personal@example.com".to_string(),
            address: "personal@example.com".to_string(),
        };

        let folders = vec![
            Folder {
                id: 1,
                account_id: 1,
                name: "INBOX".to_string(),
                unread: 42,
            },
            Folder {
                id: 2,
                account_id: 1,
                name: "Today".to_string(),
                unread: 0,
            },
            Folder {
                id: 3,
                account_id: 1,
                name: "Starred".to_string(),
                unread: 0,
            },
            Folder {
                id: 4,
                account_id: 1,
                name: "Sent".to_string(),
                unread: 0,
            },
            Folder {
                id: 5,
                account_id: 1,
                name: "Drafts".to_string(),
                unread: 0,
            },
            Folder {
                id: 6,
                account_id: 1,
                name: "Archive".to_string(),
                unread: 0,
            },
            Folder {
                id: 7,
                account_id: 1,
                name: "Work/INBOX".to_string(),
                unread: 3,
            },
            Folder {
                id: 8,
                account_id: 1,
                name: "Work/Sent".to_string(),
                unread: 0,
            },
        ];

        let messages = vec![
            MessageSummary {
                id: 1,
                folder_id: 1,
                time: "10:31".to_string(),
                from: "Alex Chen".to_string(),
                subject: "Re: Proposal".to_string(),
                unread: true,
                preview: "Thanks—attached is the updated…".to_string(),
            },
            MessageSummary {
                id: 2,
                folder_id: 1,
                time: "09:58".to_string(),
                from: "GitHub".to_string(),
                subject: "Security alert".to_string(),
                unread: true,
                preview: "We detected a new sign-in…".to_string(),
            },
            MessageSummary {
                id: 3,
                folder_id: 1,
                time: "09:12".to_string(),
                from: "HR".to_string(),
                subject: "Benefits 2026".to_string(),
                unread: false,
                preview: "Open enrollment starts…".to_string(),
            },
            MessageSummary {
                id: 4,
                folder_id: 1,
                time: "08:44".to_string(),
                from: "Newsletter".to_string(),
                subject: "Weekly digest".to_string(),
                unread: true,
                preview: "Top stories this week…".to_string(),
            },
            MessageSummary {
                id: 5,
                folder_id: 1,
                time: "Yesterday".to_string(),
                from: "Billing".to_string(),
                subject: "Invoice #1931".to_string(),
                unread: false,
                preview: "Your invoice is ready…".to_string(),
            },
            MessageSummary {
                id: 6,
                folder_id: 1,
                time: "17:22".to_string(),
                from: "Sam".to_string(),
                subject: "Lunch?".to_string(),
                unread: false,
                preview: "Want to grab lunch…".to_string(),
            },
        ];

        let message_meta = MessageViewMeta {
            subject: "Re: Proposal".to_string(),
            from: "Alex Chen <alex@…>".to_string(),
            date: "2026-02-03 10:31".to_string(),
        };

        Self {
            account,
            folders,
            messages,
            message_meta,
        }
    }
}
