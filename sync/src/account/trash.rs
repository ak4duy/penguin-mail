//! A disk-backed Trash snapshot, checked again one delete batch at a time.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use mailrs_domain::{AccountId, Role, Target};
use mailrs_store::{Result as StoreResult, StoreError};
use rusqlite::{Connection, params};

use super::AccountSync;
use crate::{BackendError, Failure, Outcome, Permitted, RemoteRef, SyncError};

const BATCH: usize = 100;

/// Counts and the first error, without retaining every deleted message.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Emptied {
    pub deleted: usize,
    pub failed: usize,
    pub error: Option<String>,
    pub needs_permission: bool,
}

impl Emptied {
    fn add(&mut self, outcome: &Outcome) {
        self.deleted += outcome.done.len();
        self.failed += outcome.failed.len();
        if self.error.is_none() {
            self.error = outcome.first_error().map(str::to_owned);
        }
    }
}

impl AccountSync {
    pub(crate) async fn empty_trash(
        &self,
        mut finished: impl FnMut(&Outcome) + Send,
    ) -> Result<Permitted<Emptied>, SyncError> {
        if !self.mail.capabilities().delete_forever {
            return Err(BackendError::Unsupported.into());
        }
        let moves = self.mail.hold_moves().await;
        let Some(trash) = self.mail.mailbox_for(Role::Trash) else {
            return Ok(Permitted::Done(Emptied::default()));
        };
        let snapshot = Arc::new(Snapshot::new()?);
        if matches!(self.services.mail, crate::AnyMail::Pop3(_)) {
            let (snapshot, trash, account) =
                (Arc::clone(&snapshot), trash.clone(), self.account_id);
            self.db
                .read(move |c| snapshot.local(c, account, &trash))
                .await?;
        } else {
            let mut cursor = None;
            loop {
                let page = self
                    .mail
                    .mailbox_page(&trash, cursor.as_deref(), 500)
                    .await?;
                snapshot.append(&page.refs)?;
                let Some(next) = page.next else { break };
                if !snapshot.cursor(&next)? {
                    return Err(BackendError::StateLost.into());
                }
                cursor = Some(next);
            }
        }
        // Listing and metadata errors must surface before the first irreversible write.
        let limit = BATCH.min(self.mail.capabilities().batch_limit.max(1));
        let mut after = 0;
        loop {
            let (last, refs) = snapshot.batch(after, limit, false)?;
            if refs.is_empty() {
                break;
            }
            let targets = self.trash_targets(&refs, &trash).await?;
            snapshot.checked(&targets)?;
            after = last;
        }
        let mut result = Emptied::default();
        after = 0;
        loop {
            let (last, refs) = snapshot.batch(after, limit, true)?;
            if refs.is_empty() {
                break;
            }
            // Another client can restore mail while preflight reads the other batches.
            let targets = match self.trash_targets(&refs, &trash).await {
                Ok(targets) => targets,
                Err(error) => {
                    result.error.get_or_insert_with(|| error.to_string());
                    break;
                }
            };
            let ids = targets
                .iter()
                .map(|t| t.message_id.iter().cloned().collect())
                .collect();
            let results = match self.erase_ids(&targets, ids, &moves, Some(&trash)).await {
                Ok(results) => results,
                Err(error) => {
                    result.error.get_or_insert_with(|| error.to_string());
                    break;
                }
            };
            let mut outcome = Outcome::default();
            for (target, answer) in targets.into_iter().zip(results) {
                match answer {
                    Ok(()) => outcome.done.push(target),
                    Err(error) => {
                        result.needs_permission |=
                            matches!(error, SyncError::Backend(BackendError::NeedsPermission));
                        outcome.failed.push(Failure {
                            target,
                            error: error.to_string(),
                        });
                    }
                }
            }
            finished(&outcome);
            result.add(&outcome);
            if result.needs_permission {
                break;
            }
            after = last;
        }
        if result.needs_permission && result.deleted == 0 {
            Ok(Permitted::NeedsPermission)
        } else {
            Ok(Permitted::Done(result))
        }
    }

    async fn trash_targets(
        &self,
        refs: &[RemoteRef],
        trash: &str,
    ) -> Result<Vec<Target>, SyncError> {
        let fetched = self
            .fetch(refs.iter().cloned().map(crate::Want::from).collect())
            .await?;
        let wanted: HashSet<_> = refs.iter().map(|hit| hit.id.as_str()).collect();
        let still_here: HashSet<_> = fetched
            .metas
            .iter()
            .filter(|m| {
                wanted.contains(m.id.as_str()) && m.held.mailboxes.iter().any(|id| id == trash)
            })
            .map(|m| m.id.clone())
            .collect();
        let account_id = self.account_id;
        let metas = self
            .db
            .write(move |c| fetched.store_missing(c, account_id).map(|(metas, _)| metas))
            .await?;
        Ok(metas
            .into_iter()
            .filter(|m| still_here.contains(&m.id))
            .map(|m| Target {
                account_id,
                thread_id: m.thread_id,
                message_id: Some(m.id),
            })
            .collect())
    }
}

/// SQLite's unnamed temporary database is deleted on close, including on cancellation.
struct Snapshot(Mutex<Connection>);

impl Snapshot {
    fn new() -> StoreResult<Self> {
        let c = Connection::open("")?;
        c.execute_batch("PRAGMA temp_store = FILE; PRAGMA cache_size = -1024;
            CREATE TABLE refs (seq INTEGER PRIMARY KEY, id TEXT UNIQUE, thread TEXT, checked INTEGER DEFAULT 0);
            CREATE TABLE cursors (value TEXT PRIMARY KEY);")?;
        Ok(Self(Mutex::new(c)))
    }

    fn append(&self, refs: &[RemoteRef]) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        for hit in refs {
            tx.execute(
                "INSERT OR IGNORE INTO refs (id, thread) VALUES (?1, ?2)",
                params![hit.id, hit.thread_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn local(&self, source: &Connection, account: AccountId, trash: &str) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        let mut query = source.prepare(
            "SELECT m.id, m.thread_id FROM messages m
            JOIN message_mailboxes f ON f.account_id = m.account_id AND f.message_id = m.id
            JOIN mailboxes b ON b.key = f.mailbox WHERE m.account_id = ?1 AND b.id = ?2",
        )?;
        let mut rows = query.query(params![account, trash])?;
        while let Some(row) = rows.next()? {
            tx.execute(
                "INSERT OR IGNORE INTO refs (id, thread) VALUES (?1, ?2)",
                params![row.get::<_, String>(0)?, row.get::<_, String>(1)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn cursor(&self, cursor: &str) -> StoreResult<bool> {
        Ok(self
            .0
            .lock()
            .expect("snapshot poisoned")
            .execute("INSERT OR IGNORE INTO cursors VALUES (?1)", [cursor])?
            != 0)
    }

    fn batch(&self, after: i64, limit: usize, checked: bool) -> StoreResult<(i64, Vec<RemoteRef>)> {
        let c = self.0.lock().expect("snapshot poisoned");
        let mut query = c.prepare("SELECT seq, id, thread FROM refs WHERE seq > ?1 AND (?2 = 0 OR checked = 1) ORDER BY seq LIMIT ?3")?;
        let mut rows = query.query(params![after, checked, limit as i64])?;
        let mut refs = Vec::new();
        let mut last = after;
        while let Some(row) = rows.next()? {
            last = row.get(0)?;
            refs.push(RemoteRef {
                id: row.get(1)?,
                thread_id: row.get(2)?,
            });
        }
        Ok((last, refs))
    }

    fn checked(&self, targets: &[Target]) -> StoreResult<()> {
        let mut c = self.0.lock().expect("snapshot poisoned");
        let tx = c.transaction()?;
        for target in targets {
            let id = target.message_id.as_ref().expect("message target");
            tx.execute("UPDATE refs SET checked = 1 WHERE id = ?1", [id])?;
        }
        tx.commit().map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_large_snapshot_deduplicates_and_reads_only_one_batch() {
        let snapshot = Snapshot::new().unwrap();
        for page in 0..50 {
            let refs: Vec<_> = (page * 500..(page + 1) * 500)
                .map(|n| RemoteRef {
                    id: format!("m{n}"),
                    thread_id: format!("t{n}"),
                })
                .collect();
            snapshot.append(&refs).unwrap();
            snapshot.append(&refs).unwrap();
        }
        assert!(snapshot.cursor("page-2").unwrap());
        assert!(!snapshot.cursor("page-2").unwrap());
        let mut after = 0;
        let mut count = 0;
        loop {
            let (last, refs) = snapshot.batch(after, BATCH, false).unwrap();
            if refs.is_empty() {
                break;
            }
            assert!(refs.len() <= BATCH);
            count += refs.len();
            after = last;
        }
        assert_eq!(count, 25_000);
    }
}
