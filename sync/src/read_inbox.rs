//! Marks each account's inbox read: every unread message in it, on the
//! server as well as on this computer, a page at a time. The tray's Mark
//! All as Read runs this across the accounts.

use std::collections::HashSet;

use mailrs_domain::query::{Query, Term};
use mailrs_domain::{AccountId, MailSet, Role, Target};
use mailrs_store::threads;

use crate::{
    Accounts, History, MailAction, MailActions, MovedFrom, Recorded, Searched, SyncError,
    TriageAction,
};

/// How many messages one page marks read.
pub const READ_PAGE: usize = 100;

/// The most messages one listing asks for: Graph answers no more. A page
/// and the mail it steps past that stays unread share it, which bounds
/// what the run keeps of an account at once.
const MOST_LISTED: usize = 250;

/// What marking the inboxes read came to. It counts rather than lists the
/// mail, so the run keeps no more than a listing's ids whatever the size
/// of the inboxes. Undo keeps what it needs to put each message back.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboxesRead {
    /// Messages that went read.
    pub read: usize,
    /// Messages that stayed unread because the change failed.
    pub refused: usize,
    /// Per account, why its unread mail could not be listed, or the first
    /// reason a message of it stayed unread.
    pub failed: Vec<(AccountId, String)>,
    /// The one undo entry every page went into.
    pub recorded: Option<Recorded>,
}

impl<A: Accounts> MailActions<A> {
    /// Marks every unread message in each account's inbox read, including
    /// mail older than this computer keeps, in pages of [`READ_PAGE`].
    /// With `History::Record` every page of every account goes into one
    /// undo entry, so one Undo takes the whole run back.
    pub async fn read_inboxes(&self, accounts: &[AccountId], history: History) -> InboxesRead {
        self.read_inboxes_in_pages(accounts, history, READ_PAGE)
            .await
    }

    pub(crate) async fn read_inboxes_in_pages(
        &self,
        accounts: &[AccountId],
        history: History,
        page: usize,
    ) -> InboxesRead {
        let mut read = InboxesRead::default();
        for &account_id in accounts {
            if let Err(err) = self.read_inbox(account_id, history, page, &mut read).await {
                read.failed.push((account_id, err.to_string()));
            }
        }
        read
    }

    /// Lists the inbox's unread mail from the top again after each page,
    /// since what went read leaves the listing. A message that comes back
    /// was refused or not yet seen as read by the search, and the next
    /// listing reaches past it rather than trying it again.
    async fn read_inbox(
        &self,
        account_id: AccountId,
        history: History,
        page: usize,
        read: &mut InboxesRead,
    ) -> Result<(), SyncError> {
        let mut tried: HashSet<String> = HashSet::new();
        let mut refusal = None;
        let listing = loop {
            let limit = (page + tried.len()).min(MOST_LISTED);
            let listed = match self.unread_in_inbox(account_id, limit).await {
                Ok(listed) => listed,
                Err(err) => break Err(err),
            };
            let more = listed.len() >= limit;
            let ids: HashSet<&str> = listed
                .iter()
                .filter_map(|t| t.message_id.as_deref())
                .collect();
            tried.retain(|id| ids.contains(id.as_str()));
            let fresh: Vec<Target> = listed
                .into_iter()
                .filter(|t| t.message_id.as_ref().is_some_and(|id| !tried.contains(id)))
                .collect();
            if fresh.is_empty() {
                if more {
                    tracing::warn!(
                        account = account_id,
                        "stopped marking the inbox read behind a listing of mail that stays unread"
                    );
                }
                break Ok(());
            }
            tried.extend(fresh.iter().filter_map(|t| t.message_id.clone()));
            let ran = self
                .run_joining(
                    &fresh,
                    MailAction::Triage(TriageAction::MarkRead),
                    history,
                    &MovedFrom::nowhere(),
                    read.recorded,
                )
                .await;
            read.read += ran.done.len();
            read.refused += ran.failed.len();
            if refusal.is_none() {
                refusal = ran.failed.into_iter().next().map(|f| f.error);
            }
            read.recorded = ran.recorded.or(read.recorded);
            if !more {
                break Ok(());
            }
        };
        if let Some(error) = refusal {
            read.failed.push((account_id, error));
        }
        listing
    }

    /// At most `limit` unread messages in the account's inbox, as the
    /// server holds them, so mail older than the store keeps is among
    /// them. An account whose mailboxes live on this computer, or whose
    /// server cannot search for unread inbox mail, answers from the store.
    async fn unread_in_inbox(
        &self,
        account_id: AccountId,
        limit: usize,
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
            sync.search_tree(&query, limit).await?
        };
        let found: Vec<(String, String)> = if searched.store_only {
            let limit = i64::try_from(limit).unwrap_or(i64::MAX);
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
