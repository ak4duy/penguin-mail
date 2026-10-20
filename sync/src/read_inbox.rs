//! Marks each account's inbox read: every unread message in it, on the
//! server as well as on this computer, a page at a time. The tray's Mark
//! All as Read runs this across the accounts.

use std::collections::HashSet;

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{AccountId, MailSet, Role, Target};
use mailrs_store::threads;

use crate::{
    Accounts, History, MailAction, MailActions, Outcome, Searched, SyncError, TriageAction,
};

/// How many messages one page marks read. Memory holds at most one page
/// of ids at a time whatever the size of the inboxes.
pub const READ_PAGE: usize = 200;

impl<A: Accounts> MailActions<A> {
    /// Marks every unread message in each account's inbox read, including
    /// mail older than this computer keeps, in pages of [`READ_PAGE`].
    /// Each page goes on the undo stack as its own action when `history`
    /// records it, so a run that fits in one page is undone as one across
    /// every account.
    pub async fn read_inboxes(&self, accounts: &[AccountId], history: History) -> Outcome {
        self.read_inboxes_in_pages(accounts, history, READ_PAGE)
            .await
    }

    pub(crate) async fn read_inboxes_in_pages(
        &self,
        accounts: &[AccountId],
        history: History,
        page: usize,
    ) -> Outcome {
        let mut outcome = Outcome::default();
        // A message that stays unread, because the server refused the
        // change or its search has not caught up, comes back on the next
        // page. An account whose page offers nothing new drops out rather
        // than being asked forever.
        let mut offered = HashSet::new();
        let mut remaining: Vec<AccountId> = accounts.to_vec();

        while !remaining.is_empty() {
            let mut batch = Vec::new();
            let mut next_remaining = Vec::new();

            for &account_id in &remaining {
                let room = page.saturating_sub(batch.len());
                if room == 0 {
                    next_remaining.push(account_id);
                    continue;
                }
                let found = match self.unread_in_inbox(account_id, room).await {
                    Ok(found) => found,
                    Err(err) => {
                        tracing::warn!(
                            account = account_id,
                            error = %err,
                            "could not list unread inbox mail"
                        );
                        continue;
                    }
                };
                let full_page = found.len() >= room;
                let fresh: Vec<Target> = found
                    .into_iter()
                    .filter(|target| offered.insert(target.clone()))
                    .collect();
                if !fresh.is_empty() && full_page {
                    next_remaining.push(account_id);
                }
                batch.extend(fresh);
            }

            if batch.is_empty() {
                break;
            }
            let ran = self
                .run(&batch, MailAction::Triage(TriageAction::MarkRead), history)
                .await;
            outcome.done.extend(ran.done);
            outcome.failed.extend(ran.failed);
            outcome.recorded = ran.recorded.or(outcome.recorded);
            remaining = next_remaining;
        }
        outcome
    }

    /// At most `page` unread messages in the account's inbox, as the
    /// server holds them, so mail older than the store keeps is among
    /// them. An account whose mailboxes live on this computer, or whose
    /// server cannot search for unread inbox mail, answers from the store.
    async fn unread_in_inbox(
        &self,
        account_id: AccountId,
        page: usize,
    ) -> Result<Vec<Target>, SyncError> {
        let sync = self
            .accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        let searched = if sync.services().capabilities().local_mailboxes {
            Searched {
                refs: Vec::new(),
                store_only: true,
            }
        } else {
            let query = Query::And(vec![
                Query::is_in(MailSet::Role(Role::Inbox)),
                Query::term(Term::Unread),
            ]);
            sync.search_tree(&query, page).await?
        };
        let found: Vec<(String, String)> = if searched.store_only {
            let limit = i64::try_from(page).unwrap_or(i64::MAX);
            self.db
                .read(move |c| threads::unread_inbox_ids(c, account_id, limit))
                .await?
        } else {
            searched
                .refs
                .into_iter()
                .map(|found| (found.thread_id, found.id))
                .collect()
        };
        Ok(found
            .into_iter()
            .map(|(thread_id, id)| Target {
                account_id,
                thread_id,
                message_id: Some(id),
            })
            .collect())
    }
}
