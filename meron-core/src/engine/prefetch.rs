//! Background body prefetch so recent mail is readable offline.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use crate::{imap, parse, store};

use super::*;

/// How far back the body prefetcher reaches: unread mail of any age, plus
/// everything received within this many days.
pub const PREFETCH_DAYS: u32 = 14;

/// Messages downloaded per IMAP session checkout. The session goes back to the
/// pool between batches so opening a message can reuse it instead of waiting
/// out the rest of the backlog or paying for a new TLS handshake.
const PREFETCH_BATCH: usize = 4;

/// One batch is a few full messages. Long enough for a slow server, short
/// enough that a dead socket is not held for the whole backlog.
const PREFETCH_BATCH_BUDGET: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct BodyPrefetchOptions {
    pub days: u32,
    pub max_count: Option<usize>,
    pub media_root: PathBuf,
}

impl Default for BodyPrefetchOptions {
    fn default() -> Self {
        Self {
            days: PREFETCH_DAYS,
            max_count: None,
            media_root: parse::media_root(),
        }
    }
}

pub(super) fn limit_prefetch_uids(mut pending: Vec<u32>, max_count: Option<usize>) -> Vec<u32> {
    // SEARCH returns ascending UIDs. The reader opens the newest mail first, so
    // warm that end before older messages. A cap (mobile) then keeps only it.
    pending.reverse();
    if let Some(max_count) = max_count {
        pending.truncate(max_count);
    }
    pending
}

/// Download full message bodies (RFC822, attachments included) for a folder's
/// unread mail and its recent messages into the store, so opening them is instant
/// and they read offline. Searches once, then fetches newest-first in short
/// batches, returning the connection to the pool between batches. Skips messages
/// whose body is already cached, so repeat runs converge to a cheap SEARCH; a
/// run cut short resumes on the next trigger since saved bodies persist. Layered
/// over the on-demand reader, which still handles anything opened before the
/// prefetcher reaches it.
pub async fn prefetch_bodies(
    engine: &Arc<Engine>,
    account: &str,
    folder: &str,
) -> anyhow::Result<usize> {
    prefetch_bodies_with_options(engine, account, folder, BodyPrefetchOptions::default()).await
}

pub async fn prefetch_bodies_with_options(
    engine: &Arc<Engine>,
    account: &str,
    folder: &str,
    options: BodyPrefetchOptions,
) -> anyhow::Result<usize> {
    let _snapshot_guard = MessageSyncGuard::begin(engine)?;
    let uids = engine
        .with_read_session(account, |session| {
            let folder = folder.to_string();
            let days = options.days;
            Box::pin(async move { imap::search_prefetch_uids(session, &folder, days).await })
        })
        .await?;
    let pending: Vec<u32> = {
        let db = engine.db.lock().unwrap();
        uids.into_iter()
            .filter(|uid| {
                store::has_message(&db, account, folder, *uid).unwrap_or(false)
                    && !store::has_cached_body(&db, account, folder, *uid).unwrap_or(false)
            })
            .collect()
    };
    let pending = limit_prefetch_uids(pending, options.max_count);

    let mut fetched = 0usize;
    for chunk in pending.chunks(PREFETCH_BATCH) {
        let chunk = chunk.to_vec();
        let batch = engine
            .with_transfer_session(account, PREFETCH_BATCH_BUDGET, |session| {
                let folder = folder.to_string();
                let account = account.to_string();
                let chunk = chunk.clone();
                let media_root = options.media_root.clone();
                Box::pin(async move {
                    // peek via fetch_bodies: warming must not flip unread mail to read.
                    imap::fetch_bodies(session, &folder, &chunk, media_root, &account).await
                })
            })
            .await;
        match batch {
            Ok(messages) => {
                let db = engine.db.lock().unwrap();
                for (uid, message) in messages {
                    let _ = store::save_cached_message(&db, account, folder, uid, &message);
                    fetched += 1;
                }
            }
            // Saved batches stay cached; the next trigger continues with the rest.
            Err(e) => {
                eprintln!("meron-core: prefetch {folder}: {e:#}");
                break;
            }
        }
    }
    Ok(fetched)
}

/// Fetch and cache the bodies of specific UIDs, skipping any already cached.
///
/// Unlike [`prefetch_bodies`] this takes the UIDs from the caller instead of a
/// server-side SEARCH, because the caller (new-mail notification) already knows
/// exactly which messages it needs and awaits the result — a SEARCH round trip
/// per arrival would add latency to the notification for nothing.
pub async fn fetch_bodies_for_uids(
    engine: &Arc<Engine>,
    account: &str,
    folder: &str,
    uids: &[u32],
    media_root: PathBuf,
) -> anyhow::Result<usize> {
    let pending: Vec<u32> = {
        let db = engine.db.lock().unwrap();
        uids.iter()
            .copied()
            .filter(|uid| !store::has_cached_body(&db, account, folder, *uid).unwrap_or(false))
            .collect()
    };
    if pending.is_empty() {
        return Ok(0);
    }
    let _snapshot_guard = MessageSyncGuard::begin(engine)?;
    engine
        .with_transfer_session(account, PREFETCH_BATCH_BUDGET, |session| {
            let engine = engine.clone();
            let account = account.to_string();
            let folder = folder.to_string();
            let pending = pending.clone();
            let media_root = media_root.clone();
            Box::pin(async move {
                let fetched =
                    imap::fetch_bodies(session, &folder, &pending, media_root, &account).await?;
                let count = fetched.len();
                let db = engine.db.lock().unwrap();
                for (uid, message) in fetched {
                    let _ = store::save_cached_message(&db, &account, &folder, uid, &message);
                }
                anyhow::Ok(count)
            })
        })
        .await
}

/// Warm a folder's bodies in the background (deduped per account/folder). Stays
/// silent: this is an optimization, so failures go to stderr rather than
/// surfacing as UI error toasts. The stored data it fills is observed lazily when the
/// user opens a message.
pub fn spawn_body_prefetch(engine: Arc<Engine>, account: String, folder: String) {
    if engine.is_paused(&account) {
        return;
    }
    let key = format!("body:{account}/{folder}");
    if !engine.syncing.lock().unwrap().insert(key.clone()) {
        return;
    }
    tokio::spawn(async move {
        let result = tokio::time::timeout(
            Duration::from_secs(600),
            prefetch_bodies(&engine, &account, &folder),
        )
        .await;
        engine.syncing.lock().unwrap().remove(&key);
        match result {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("meron-core: prefetch {folder}: {e:#}"),
            // Partial progress persisted; the next trigger resumes the rest.
            Err(_) => eprintln!("meron-core: prefetch {folder}: timed out (will resume)"),
        }
    });
}

/// Rows per [`store::backfill_files_batch`] call: small enough that the store
/// is never held long enough to stall a UI request queued behind it.
pub const FILES_BACKFILL_BATCH: u32 = 500;

/// Fill `messages.files` for mail cached before the column existed, one short
/// batch at a time off the async runtime, releasing the store between batches.
/// A no-op once finished; an interrupted run resumes on the next start.
pub fn spawn_files_backfill(engine: Arc<Engine>) {
    tokio::task::spawn_blocking(move || {
        loop {
            let result = {
                let db = crate::log::timed_db_lock(&engine.db, "files_backfill");
                store::backfill_files_batch(&db, FILES_BACKFILL_BATCH)
            };
            match result {
                Ok(true) => return,
                Ok(false) => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    eprintln!("meron-core: attachment backfill: {e:#}");
                    return;
                }
            }
        }
    });
}
