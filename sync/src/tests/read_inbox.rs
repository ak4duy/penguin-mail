use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use mailrs_domain::mailbox::keyword::SEEN;
use mailrs_domain::{MailSet, MessageMeta, RemoveSetting, Role};
use mailrs_store::accounts;

use super::microsoft::outlook;
use super::pop3::pop3_harness;
use super::{Connected, harness};
use crate::fake::{FakeGmail, FakeMail, FakePop3, has_label, meta, pop3_mail};
use crate::{AccountServices, AccountSync, History, MailActions, now_millis};

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
        .read_inboxes_in_pages(&[h.account_id], History::Record, 2)
        .await;

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

/// A server that cannot search for unread mail (Graph) and a local-mailbox
/// account (POP3) both fall back to the store's unread inbox ids.
#[tokio::test]
async fn servers_without_unread_search_and_local_mailboxes_fall_back_to_the_store() {
    let ms = outlook().await;
    let id1 = ms.fake.deliver(
        &ms.fake.folder_id("inbox"),
        FakeMail {
            at: now_millis(),
            ..FakeMail::default()
        },
    );
    ms.bootstrap_all().await;
    let ms_actions = MailActions::new(
        Arc::new(Connected(HashMap::from([(
            ms.account_id,
            Arc::clone(&ms.sync),
        )]))),
        ms.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    );
    let ms_outcome = ms_actions
        .read_inboxes_in_pages(&[ms.account_id], History::Record, 2)
        .await;
    assert_eq!(ms_outcome.done.len(), 1);
    assert!(ms.held(&id1).await.keywords.contains(&SEEN.to_string()));

    let pop = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let pop_id = pop.keep("u1", &pop3_mail(1), "inbox", now_millis()).await;
    let pop_actions = MailActions::new(
        Arc::new(Connected(HashMap::from([(
            pop.account_id,
            Arc::clone(&pop.sync),
        )]))),
        pop.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    );
    let pop_outcome = pop_actions
        .read_inboxes_in_pages(&[pop.account_id], History::Record, 2)
        .await;
    assert_eq!(pop_outcome.done.len(), 1);
    assert!(!pop.ids_in(MailSet::Unseen).await.contains(&pop_id));
}

/// Unread mail in two accounts that fits in one page is one action, so
/// one Undo makes both accounts' mail unread again.
#[tokio::test]
async fn one_undo_takes_back_every_account() {
    let h = harness().await;
    h.fake
        .seed(meta("a", "t1", now_millis(), &["INBOX", "UNREAD"]));
    h.bootstrap_all().await;
    let other_id =
        h.db.write(|c| accounts::insert_account(c, "you@example.com", 0))
            .await
            .unwrap();
    let other_fake = Arc::new(FakeGmail::new());
    other_fake.seed(MessageMeta {
        account_id: other_id,
        ..meta("x", "t2", now_millis(), &["INBOX", "UNREAD"])
    });
    other_fake.with(|s| s.page_size = 1000);
    let (sender, _events) = async_channel::unbounded();
    let other = Arc::new(
        AccountSync::new(
            other_id,
            AccountServices::google(Arc::clone(&other_fake)),
            h.db.clone(),
            sender,
        )
        .with_retry_max(Duration::from_millis(10)),
    );
    other.bootstrap().await.unwrap();
    let actions = MailActions::new(
        Arc::new(Connected(HashMap::from([
            (h.account_id, Arc::clone(&h.sync)),
            (other_id, other),
        ]))),
        h.db.clone(),
        crate::OneClick::Fake(Arc::default()),
    );
    let mine = || h.fake.with(|s| has_label(&s.messages["a"], "UNREAD"));
    let theirs = || other_fake.with(|s| has_label(&s.messages["x"], "UNREAD"));

    let outcome = actions
        .read_inboxes(&[h.account_id, other_id], History::Record)
        .await;
    assert_eq!(outcome.done.len(), 2, "{:?}", outcome.failed);
    assert!(!mine() && !theirs());

    actions.undo().await.expect("an undo");
    assert!(mine() && theirs(), "one undo reaches both accounts");
    assert!(actions.undo().await.is_none(), "it was one action");
}
