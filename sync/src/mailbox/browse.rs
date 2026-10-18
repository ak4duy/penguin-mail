//! Paged explicit server mailboxes, separate from background Follow and
//! from the capped searches used by the other remote listings.

use super::*;
use crate::BackendError;
use mailrs_store::messages;

/// The loaded prefix of one server mailbox. Only the cursor says whether
/// the server has more: a cache miss or a page of replies says nothing
/// about the end of the mailbox.
#[derive(Clone)]
pub(super) struct Browsed {
    changes: u64,
    ids: Vec<String>,
    next: Option<String>,
    started: bool,
}

#[derive(Default)]
pub(super) struct Browser {
    revision: u64,
    shown: Option<Mailbox>,
    listings: HashMap<(AccountId, String), Browsed>,
}

impl Browsed {
    fn new(changes: u64) -> Self {
        Self {
            changes,
            ids: Vec::new(),
            next: None,
            started: false,
        }
    }
}

pub(super) struct Browse<'a> {
    pub shown: &'a Mailbox,
    pub mailbox: &'a Mailbox,
    pub revision: Option<u64>,
}

impl<A: Accounts> Mailboxes<A> {
    pub(super) async fn browse(
        &self,
        request: Browse<'_>,
        scope: &Scope,
        view: &View,
        base: Listing,
        held: Loaded,
        stop: &Stop,
    ) -> Result<Listing, SyncError> {
        let Browse {
            shown,
            mailbox,
            revision,
        } = request;
        let revision = revision.unwrap_or_else(|| self.begin_browse(shown, held.count == 0));
        let Mailbox::Label {
            account_id,
            label_id,
            ..
        } = mailbox
        else {
            unreachable!()
        };
        let Some(sync) = self.accounts.account(*account_id) else {
            return self.stored(mailbox, view, base, held.last).await;
        };
        let changes = sync.mail_changes();
        // Keep no shared lock across a server or store await. A request
        // for another mailbox can supersede this one while it is waiting.
        let mut listing = {
            let mut cached = self.browsed.lock().expect("mailbox browser poisoned");
            if cached.revision != revision || cached.shown.as_ref() != Some(shown) {
                return Ok(base);
            }
            let key = (*account_id, label_id.clone());
            let entry = cached
                .listings
                .entry(key)
                .or_insert_with(|| Browsed::new(changes));
            if entry.changes != changes {
                *entry = Browsed::new(changes);
            }
            entry.clone()
        };
        if (!listing.started || listing.next.is_some()) && !stop.stopped() {
            let limit = view.limit.unwrap_or(REMOTE_PAGE).clamp(1, REMOTE_PAGE);
            let mut page = sync
                .browse_mailbox(label_id, listing.next.as_deref(), limit)
                .await;
            if matches!(page, Err(SyncError::Backend(BackendError::StateLost))) {
                // UIDVALIDITY changed. None of the old prefix names the
                // new mailbox, even when its UIDs happen to agree.
                listing = Browsed::new(sync.mail_changes());
                page = sync.browse_mailbox(label_id, None, limit).await;
            }
            match page {
                Ok(page) => {
                    listing.ids.extend(page.refs.into_iter().map(|r| r.id));
                    listing.next = page.next;
                    listing.started = true;
                }
                Err(err) => {
                    // An append failure keeps both the valid prefix and
                    // its cursor. Returning an error leaves the rows on
                    // screen and lets the same page be retried.
                    if held.count > 0 && listing.started {
                        return Err(err);
                    }
                    // A listing that started offline can still page
                    // through more of its cache. Re-read the prefix so
                    // grouping and local moves cannot append duplicates.
                    let cached_view = View {
                        limit: Some(held.count.saturating_add(view.limit.unwrap_or(PAGE))),
                        ..view.clone()
                    };
                    let mut fallback = self.stored(mailbox, &cached_view, base, None).await?;
                    let email = scope
                        .searched(Some(*account_id))
                        .first()
                        .map(|a| a.email.as_str())
                        .unwrap_or_default();
                    fallback.notices.push(load_notice(email, &err));
                    // Cached mail is not proof that the server ended.
                    fallback.more = true;
                    fallback.replace = true;
                    return Ok(fallback);
                }
            }
        }
        let (account_id, ids, label) = (*account_id, listing.ids.clone(), label_id.clone());
        let grouped = view.threading;
        let mut rows = self
            .db
            .read(move |c| {
                let hits = messages::by_ids(c, account_id, &ids)?;
                let mut rows = summarize_search(
                    hits.into_iter()
                        .filter(|m| m.held.mailboxes.contains(&label))
                        .collect(),
                    grouped,
                );
                threads::decorate_rows(c, account_id, &mut rows)?;
                Ok(rows)
            })
            .await?;
        rows.sort_by(|a, b| {
            b.last_message_at
                .cmp(&a.last_message_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        let more = !listing.started || listing.next.is_some();
        let mut cached = self.browsed.lock().expect("mailbox browser poisoned");
        if cached.revision == revision {
            cached
                .listings
                .insert((account_id, label_id.clone()), listing);
        }
        Ok(Listing {
            rows,
            more,
            replace: true,
            ..base
        })
    }

    fn begin_browse(&self, shown: &Mailbox, first: bool) -> u64 {
        let mut cached = self.browsed.lock().expect("mailbox browser poisoned");
        if first || cached.shown.as_ref() != Some(shown) {
            cached.shown = Some(shown.clone());
            cached.listings.clear();
            cached.revision = cached.revision.wrapping_add(1);
        }
        cached.revision
    }

    pub(super) fn paged_account(&self, account_id: AccountId) -> bool {
        self.accounts.services(account_id).is_some_and(|services| {
            let capabilities = services.capabilities();
            capabilities.paged_mailboxes && !capabilities.local_mailboxes
        })
    }

    pub(super) fn standard_label(&self, account_id: AccountId, which: Standard) -> Option<Mailbox> {
        let role = match which {
            Standard::Sent => Role::Sent,
            Standard::Drafts => Role::Drafts,
            _ => return None,
        };
        let services = self.accounts.services(account_id)?;
        if !services.capabilities().paged_mailboxes {
            return None;
        }
        Some(Mailbox::Label {
            account_id,
            label_id: services.mail.mailbox_for(role)?,
            name: which.name(),
        })
    }

    pub(super) async fn browse_unified(
        &self,
        which: Standard,
        scope: &Scope,
        view: &View,
        mut base: Listing,
        held: Loaded,
        stop: &Stop,
    ) -> Result<Listing, SyncError> {
        let shown = Mailbox::Unified(which);
        // One revision covers the whole request. A late account answer
        // must not start another account under a superseded view.
        let revision = self.begin_browse(&shown, held.count == 0);
        let cached_view = View {
            limit: Some(held.count.saturating_add(view.limit.unwrap_or(PAGE)).max(1)),
            ..view.clone()
        };
        for account in scope.searched(None) {
            if self
                .browsed
                .lock()
                .expect("mailbox browser poisoned")
                .revision
                != revision
                || stop.stopped()
            {
                break;
            }
            let individual = Mailbox::Standard {
                account_id: account.id,
                which,
            };
            let page = if let Some(label) = self.standard_label(account.id, which) {
                self.browse(
                    Browse {
                        shown: &shown,
                        mailbox: &label,
                        revision: Some(revision),
                    },
                    scope,
                    view,
                    Listing::default(),
                    held.clone(),
                    stop,
                )
                .await?
            } else {
                self.stored(&individual, &cached_view, Listing::default(), None)
                    .await?
            };
            base.rows.extend(page.rows);
            base.more |= page.more;
            base.unread += page.unread;
            base.notices.extend(page.notices);
        }
        base.rows.sort_by(|a, b| {
            b.last_message_at
                .cmp(&a.last_message_at)
                .then_with(|| a.account_id.cmp(&b.account_id))
                .then_with(|| a.id.cmp(&b.id))
        });
        base.replace = true;
        Ok(base)
    }
}
