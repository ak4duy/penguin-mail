use std::collections::HashMap;
use std::sync::Arc;

use mailrs_domain::{MailSet, Role};

use super::{Connected, harness};
use crate::fake::{has_label, meta};
use crate::{History, MailActions, now_millis};

const DAY: i64 = 24 * 60 * 60 * 1000;

/// Unread inbox mail goes read on Gmail in pages, including a message
/// older than the store holds, while read mail and unread mail outside
/// the inbox stay as they were.
#[tokio::test]
async fn the_inbox_goes_read_on_the_server_past_what_the_store_holds() {
    let h = harness().await;
    let now = now_millis();
    h.fake.seed(meta("a", "t1", now, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("b", "t2", now - 1, &["INBOX", "UNREAD"]));
    h.fake.seed(meta("r", "t3", now - 2, &["INBOX"]));
    h.fake.seed(meta("x", "t4", now - 3, &["UNREAD"]));
    h.bootstrap_all().await;
    // Seeded after the first sync and with no history, as mail is that
    // the store never fetched.
    h.fake
        .seed(meta("old", "t5", now - 400 * DAY, &["INBOX", "UNREAD"]));
    assert!(
        !h.threads(MailSet::Role(Role::Inbox))
            .await
            .contains(&"t5".to_string())
    );
    let actions = MailActions::new(
        Arc::new(Connected(HashMap::from([(
            h.account_id,
            Arc::clone(&h.sync),
        )]))),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    );

    let outcome = actions
        .read_inbox_in_pages(h.account_id, History::Record, 2)
        .await
        .unwrap();

    assert!(outcome.failed.is_empty(), "{:?}", outcome.failed);
    let mut done: Vec<_> = outcome
        .done
        .iter()
        .filter_map(|t| t.message_id.clone())
        .collect();
    done.sort();
    assert_eq!(done, ["a", "b", "old"]);
    let unread = |id: &str| h.fake.with(|s| has_label(&s.messages[id], "UNREAD"));
    for id in ["a", "b", "old", "r"] {
        assert!(!unread(id), "{id} is read on Gmail");
    }
    assert!(unread("x"), "unread mail outside the inbox stays unread");
    assert!(!h.labels_of("a").await.contains(&"UNREAD".to_string()));
}
