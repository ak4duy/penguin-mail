//! First loads commit only after the whole nested mailbox reaches the store.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use mailrs_imap::ImapError;
use mailrs_store::{Db, StoreError};

use super::{days_ago, fill, message};
use crate::fake::FakeImap;
use crate::tests::{ImapHarness, fake_settings, imap_harness_on};
use crate::{ALL_MAIL, BackendError, DEFAULT_BODY_CACHE_BYTES, SyncError};

const MAILBOX: &str = "Work/Clients";

/// Both layers must use the same window: foreground listings use the
/// account's limit, while the adapter's feed uses its own settings.
async fn nested(delimiter: char, days: i64, age: i64, count: u32) -> ImapHarness {
    let imap = FakeImap::new();
    let mailbox = format!("Work{delimiter}Clients");
    imap.add_mailbox("Work", None);
    imap.add_mailbox(&mailbox, None);
    imap.with(|s| {
        s.delimiter = delimiter;
        s.mailbox_mut("Work").no_select = true;
    });
    for n in 0..count {
        imap.deliver_flagged(
            &mailbox,
            &message(&format!("client{n}"), "Plans", ""),
            &[],
            days_ago(age),
        );
    }
    let mut settings = fake_settings();
    settings.window_days = days;
    let mut h = imap_harness_on(imap, settings).await;
    let sync = Arc::try_unwrap(h.sync).unwrap_or_else(|_| panic!("one sync owner"));
    h.sync = Arc::new(sync.with_limits(days, DEFAULT_BODY_CACHE_BYTES));
    h.bootstrap().await;
    assert!(h.ids().await.is_empty(), "unopened folders stay unloaded");
    h
}

fn ids(h: &ImapHarness, mailbox: &str, count: u32) -> Vec<String> {
    let validity = h.imap.with(|s| s.mailbox_mut(mailbox).uidvalidity);
    let mut ids: Vec<_> = (1..=count)
        .map(|uid| format!("{mailbox}/{validity}/{uid}"))
        .collect();
    ids.sort();
    ids
}

async fn unread_headers_only(h: &ImapHarness, mailbox: &str, count: u32) {
    for id in ids(h, mailbox, count) {
        assert!(h.stored(&id).await.expect("stored metadata").is_unread());
    }
    for uid in 1..=count {
        assert!(
            !h.imap
                .message(mailbox, uid)
                .unwrap()
                .flags
                .contains("\\Seen")
        );
    }
    for method in ["body", "structure", "store"] {
        assert_eq!(
            h.imap.calls_to(method),
            0,
            "metadata loading never calls {method}"
        );
    }
}

async fn failed_initial_load_is_retried(method: &str) {
    let h = nested('/', 183, 2, 2).await;
    h.imap.fail_on(
        &format!("{method} {MAILBOX}"),
        ImapError::Network("fixture first load failed".into()),
    );
    assert!(matches!(
        h.sync.follow_mailbox(MAILBOX).await,
        Err(SyncError::Backend(BackendError::Offline(_)))
    ));
    assert!(h.ids().await.is_empty());

    // A poll between attempts must not mistake the failed listing's
    // UIDNEXT for proof that the original messages reached the store.
    h.sync.incremental().await.unwrap();
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
    unread_headers_only(&h, MAILBOX, 2).await;
}

#[tokio::test]
async fn a_failed_search_does_not_consume_the_first_nested_load() {
    failed_initial_load_is_retried("search").await;
}

#[tokio::test]
async fn failed_headers_do_not_consume_the_first_nested_load() {
    failed_initial_load_is_retried("headers").await;
}

#[tokio::test]
async fn a_failed_second_header_batch_retries_all_original_messages() {
    let h = nested('/', 183, 2, 0).await;
    fill(&h.imap, MAILBOX, 501);
    h.imap.fail_on_later(
        &format!("headers {MAILBOX}"),
        1,
        ImapError::Network("fixture second batch failed".into()),
    );
    assert!(h.sync.follow_mailbox(MAILBOX).await.is_err());
    assert_eq!(h.imap.calls_to("headers"), 2);
    assert!(h.ids().await.is_empty(), "no partial batch is committed");
    h.sync.incremental().await.unwrap();
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();

    assert_eq!(h.ids().await, ids(&h, MAILBOX, 501));
    assert_eq!(h.imap.calls_to("headers"), 4);
    unread_headers_only(&h, MAILBOX, 501).await;
}

#[tokio::test]
async fn a_first_visit_repairs_a_checkpoint_saved_by_an_older_failed_load() {
    use crate::MailBackend;

    let h = nested('/', 183, 2, 2).await;
    let mail = &h.sync.services().mail;
    // Simulate the older client's premature checkpoint without fetching
    // headers. The poll then persists a cursor past these missing UIDs.
    assert!(mail.follow(MAILBOX));
    mail.window_ids(183, Some(MAILBOX)).await.unwrap();
    h.sync.incremental().await.unwrap();
    assert!(h.ids().await.is_empty());

    let sync = h.restarted().with_limits(183, DEFAULT_BODY_CACHE_BYTES);
    sync.incremental().await.unwrap();
    assert!(
        h.ids().await.is_empty(),
        "unchanged UIDs stay below the cursor"
    );
    sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
    let calls = h.imap.calls();
    sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.imap.calls(), calls, "repair happens only once this run");
    unread_headers_only(&h, MAILBOX, 2).await;
}

#[tokio::test]
async fn first_visit_before_replay_keeps_ids_bodies_and_colors_after_uidvalidity_reset() {
    use mailrs_domain::FlagColor;
    use mailrs_store::{bodies, flags};

    let h = nested('/', 183, 2, 2).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    let before = h.ids().await;
    let kept = before[0].clone();
    h.sync.body(&kept).await.unwrap();
    let (account_id, id) = (h.account_id, kept.clone());
    h.db.write(move |c| flags::set_color(c, account_id, &id, Some(&id), Some(FlagColor::Blue)))
        .await
        .unwrap();
    h.imap.reset_uidvalidity(MAILBOX);
    let validity = h.imap.with(|s| s.mailbox_mut(MAILBOX).uidvalidity);
    let sync = h.restarted().with_limits(183, DEFAULT_BODY_CACHE_BYTES);
    // Re-follow is allowed before the restored history sees the reset.
    sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, before);
    assert_eq!(
        mailrs_domain::Location::parse(&h.location(&kept).await.unwrap())
            .unwrap()
            .uidvalidity,
        validity
    );
    let (account_id, id) = (h.account_id, kept);
    h.db.read(move |c| {
        assert!(bodies::peek_body(c, account_id, &id)?.is_some());
        assert_eq!(
            flags::colors(c, account_id, &id, Some(&id))?[0].1,
            Some(FlagColor::Blue)
        );
        Ok(())
    })
    .await
    .unwrap();
    sync.incremental().await.unwrap();
    assert_eq!(
        h.ids().await,
        before,
        "later replay must not create duplicates either"
    );
}

#[tokio::test]
async fn successful_repeated_visits_fetch_nothing_and_new_mail_still_arrives() {
    let h = nested('/', 183, 2, 2).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    let calls = h.imap.calls();
    for _ in 0..3 {
        h.sync.follow_mailbox(MAILBOX).await.unwrap();
    }
    assert_eq!(h.imap.calls(), calls, "a loaded folder is idempotent");

    h.imap.deliver_flagged(
        MAILBOX,
        &message("later", "More plans", ""),
        &[],
        days_ago(0),
    );
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 3));
    unread_headers_only(&h, MAILBOX, 3).await;
}

#[tokio::test]
async fn an_offline_first_visit_returns_a_retryable_error_then_loads_online() {
    let mut h = nested('/', 183, 2, 2).await;
    let network = Arc::new(AtomicBool::new(false));
    let sync = Arc::try_unwrap(h.sync).unwrap_or_else(|_| panic!("one sync owner"));
    h.sync = Arc::new(sync.with_network(Arc::clone(&network)));
    let calls = h.imap.calls();
    let error = h
        .sync
        .follow_mailbox(MAILBOX)
        .await
        .expect_err("offline is not a completed load");
    assert!(
        matches!(error, SyncError::Backend(ref err @ BackendError::Offline(_)) if err.is_transient())
    );
    assert_eq!(h.imap.calls(), calls, "offline means no server calls");
    network.store(true, Ordering::SeqCst);
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
    unread_headers_only(&h, MAILBOX, 2).await;
}

#[tokio::test]
async fn local_pop3_folders_do_not_need_a_network_to_follow() {
    use crate::fake::{FakePop3, pop3_mail};
    use crate::tests::pop3::pop3_harness;
    use mailrs_domain::{MailSet, RemoveSetting, Role};

    let h = pop3_harness(FakePop3::default(), RemoveSetting::Never).await;
    let id = h.keep("local", &pop3_mail(1), "inbox", days_ago(1)).await;
    let sync = h.again().with_network(Arc::new(AtomicBool::new(false)));
    sync.follow_mailbox("inbox").await.unwrap();
    assert_eq!(h.ids_in(MailSet::Role(Role::Inbox)).await, [id]);
    assert_eq!(h.fake.connects(), 0);
    assert!(h.smtp.with(|s| s.sent.is_empty()));
}

#[tokio::test]
async fn a_label_backend_treats_an_offline_follow_as_a_noop() {
    let h = crate::tests::harness().await;
    let sync = h
        .sync_with(Duration::ZERO)
        .with_network(Arc::new(AtomicBool::new(false)));
    let before = h.fake.usage().calls;
    sync.follow_mailbox("Label_test").await.unwrap();
    assert_eq!(h.fake.usage().calls, before);
}

async fn healthy_nested(delimiter: char) {
    let h = nested(delimiter, 183, 2, 2).await;
    let mailbox = format!("Work{delimiter}Clients");
    h.sync.follow_mailbox(&mailbox).await.unwrap();
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ids(&h, &mailbox, 2));
    unread_headers_only(&h, &mailbox, 2).await;
    assert!(
        !h.imap
            .calls()
            .iter()
            .any(|call| call == "select Work" || call.starts_with("select Work "))
    );
}

#[tokio::test]
async fn a_healthy_nested_slash_folder_loads_recent_mail() {
    healthy_nested('/').await;
}

#[tokio::test]
async fn a_healthy_nested_dot_folder_loads_recent_mail() {
    healthy_nested('.').await;
}

#[tokio::test]
async fn old_only_nested_mail_stays_outside_a_183_day_window() {
    let h = nested('/', 183, 200, 2).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    assert!(
        h.ids().await.is_empty(),
        "a healthy old-only folder may be empty here"
    );
    assert_eq!(h.imap.calls_to("headers"), 0);
    assert!(h.imap.calls().iter().any(|call| call.starts_with(&format!("search {MAILBOX} ")) && call.contains(" SINCE ")));
}

#[tokio::test]
async fn all_mail_includes_old_only_nested_mail() {
    let h = nested('/', ALL_MAIL, 200, 2).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
    unread_headers_only(&h, MAILBOX, 2).await;
    assert!(!h.imap.calls().iter().any(|call| call.starts_with(&format!("search {MAILBOX} ")) && call.contains(" SINCE ")));
}

#[tokio::test]
async fn a_failed_store_leaves_the_first_nested_load_retriable() {
    let h = nested('/', 183, 2, 2).await;
    h.db.write(|c| {
        c.execute_batch("CREATE TRIGGER reject_follow BEFORE INSERT ON messages BEGIN SELECT RAISE(ABORT, 'fixture store failure'); END;")?;
        Ok(())
    }).await.unwrap();
    assert!(matches!(
        h.sync.follow_mailbox(MAILBOX).await,
        Err(SyncError::Store(_))
    ));
    assert!(h.ids().await.is_empty());
    h.db.write(|c| {
        c.execute_batch("DROP TRIGGER reject_follow;")?;
        Ok(())
    })
    .await
    .unwrap();
    h.sync.incremental().await.unwrap();
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
}

#[tokio::test]
async fn a_deleted_folder_can_be_followed_again_when_its_name_is_reused() {
    use crate::services::ImapApi;

    let h = nested('/', 183, 2, 0).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    h.imap.delete(MAILBOX).await.unwrap();
    h.sync.incremental().await.unwrap();
    assert!(!h.is_followed(MAILBOX));
    h.sync.refresh_labels().await.unwrap();
    assert!(h.ids().await.is_empty());

    h.imap.add_mailbox(MAILBOX, None);
    h.imap.deliver_flagged(
        MAILBOX,
        &message("replacement", "New folder", ""),
        &[],
        days_ago(1),
    );
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 1));
    unread_headers_only(&h, MAILBOX, 1).await;
}

#[tokio::test]
async fn a_new_listing_forgets_a_deleted_folders_completed_first_load() {
    use crate::services::ImapApi;

    let h = nested('/', 183, 2, 0).await;
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    h.imap.delete(MAILBOX).await.unwrap();
    // The mailbox listing can notice the deletion before a feed poll.
    h.sync.refresh_labels().await.unwrap();
    assert!(h.ids().await.is_empty());
    h.imap.add_mailbox(MAILBOX, None);
    h.imap.deliver_flagged(
        MAILBOX,
        &message("replacement", "New folder", ""),
        &[],
        days_ago(1),
    );
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 1));
    unread_headers_only(&h, MAILBOX, 1).await;
}

/// Keeps the synthetic store's writer busy until the sender drops. Reads
/// still run under WAL, so the first load can reach its final write.
async fn block_writer(
    db: &Db,
) -> (
    std::sync::mpsc::Sender<()>,
    tokio::task::JoinHandle<Result<(), StoreError>>,
) {
    let (release, wait) = std::sync::mpsc::channel();
    let (entered, ready) = tokio::sync::oneshot::channel();
    let db = db.clone();
    let task = tokio::spawn(async move {
        db.write(move |_| {
            let _ = entered.send(());
            let _ = wait.recv();
            Ok(())
        })
        .await
    });
    ready.await.unwrap();
    (release, task)
}

#[tokio::test]
async fn canceling_the_first_load_does_not_leave_a_completed_follow() {
    let h = nested('/', 183, 2, 2).await;
    let (release, writer) = block_writer(&h.db).await;
    let mut following = Box::pin(h.sync.follow_mailbox(MAILBOX));
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.imap.calls_to("headers") == 0 {
            assert!(futures::poll!(following.as_mut()).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(following);
    drop(release);
    writer.await.unwrap().unwrap();
    assert!(
        !h.is_followed(MAILBOX),
        "cancellation cannot commit the follow"
    );
    h.sync.incremental().await.unwrap();
    h.sync.follow_mailbox(MAILBOX).await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
    unread_headers_only(&h, MAILBOX, 2).await;
}

#[tokio::test]
async fn a_poll_cannot_adopt_the_first_load_before_its_store_commits() {
    let h = nested('/', 183, 2, 2).await;
    let (release, writer) = block_writer(&h.db).await;
    let mut following = Box::pin(h.sync.follow_mailbox(MAILBOX));
    tokio::time::timeout(Duration::from_secs(5), async {
        while h.imap.calls_to("headers") == 0 {
            assert!(futures::poll!(following.as_mut()).is_pending());
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let calls = h.imap.calls();
    assert!(
        tokio::time::timeout(Duration::from_millis(50), h.sync.incremental())
            .await
            .is_err()
    );
    assert_eq!(
        h.imap.calls(),
        calls,
        "poll waits for the initial hydration"
    );
    drop(release);
    writer.await.unwrap().unwrap();
    following.await.unwrap();
    h.sync.incremental().await.unwrap();
    assert_eq!(h.ids().await, ids(&h, MAILBOX, 2));
}
