//! Marks an account's inbox read: every unread message in it, on the
//! server as well as on this computer, a page at a time. The tray's Mark
//! All as Read runs this for each account.

use std::collections::HashSet;

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{AccountId, MailSet, Role, Target};
use mailrs_store::threads;

use crate::{Accounts, History, MailAction, MailActions, Outcome, SyncError, TriageAction};

/// How many messages one page marks read. A page is one search and one
/// change on the server, and memory holds one page of ids at a time
/// whatever the size of the inbox.
pub const READ_PAGE: usize = 200;

impl<A: Accounts> MailActions<A> {
    /// Marks every unread message in the account's inbox read, including
    /// mail older than this computer keeps. Each page goes on the undo
    /// stack as its own action when `history` records it.
    ///
    /// Fails only when the first page cannot be listed. A later page that
    /// cannot be listed ends the run with the pages before it done.
    pub async fn read_inbox(
        &self,
        account_id: AccountId,
        history: History,
    ) -> Result<Outcome, SyncError> {
        self.read_inbox_in_pages(account_id, history, READ_PAGE)
            .await
    }

    pub(crate) async fn read_inbox_in_pages(
        &self,
        account_id: AccountId,
        history: History,
        page: usize,
    ) -> Result<Outcome, SyncError> {
        let mut outcome = Outcome::default();
        // A message that stays unread, because the server refused the
        // change or its search has not caught up, comes back on the next
        // page. A page that offers nothing new ends the run rather than
        // asking forever.
        let mut offered = HashSet::new();
        loop {
            let found = match self.unread_in_inbox(account_id, page).await {
                Ok(found) => found,
                Err(err) if offered.is_empty() => return Err(err),
                Err(err) => {
                    tracing::warn!(account = account_id, error = %err, "stopped marking the inbox read");
                    break;
                }
            };
            let last = found.len() < page;
            let fresh: Vec<Target> = found
                .into_iter()
                .filter(|target| offered.insert(target.clone()))
                .collect();
            if fresh.is_empty() {
                break;
            }
            let ran = self
                .run(&fresh, MailAction::Triage(TriageAction::MarkRead), history)
                .await;
            outcome.done.extend(ran.done);
            outcome.failed.extend(ran.failed);
            outcome.recorded = ran.recorded.or(outcome.recorded);
            if last {
                break;
            }
        }
        Ok(outcome)
    }

    /// At most `page` unread messages in the account's inbox, as the
    /// server holds them, so mail older than the store keeps is among
    /// them. A server that cannot search for unread inbox mail leaves the
    /// store's copy to answer.
    async fn unread_in_inbox(
        &self,
        account_id: AccountId,
        page: usize,
    ) -> Result<Vec<Target>, SyncError> {
        let sync = self
            .accounts
            .account(account_id)
            .ok_or(SyncError::UnknownAccount(account_id))?;
        let query = Query::And(vec![
            Query::is_in(MailSet::Role(Role::Inbox)),
            Query::term(Term::Unread),
        ]);
        let searched = sync.search_tree(&query, page).await?;
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
