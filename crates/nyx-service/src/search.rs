//! Tree search: the server-side `find` fast path and the client-walk
//! fallback the dispatcher spawns for `SearchTree`.
//!
//! Pure mechanical move out of `lib.rs` (code review 2026-06-08, plan 05).

use super::*;

/// The one capability a tree search needs: list a directory. Going through a
/// narrow trait (rather than `RemoteClient` directly) keeps [`run_search`]
/// unit-testable against an in-memory fake.
#[async_trait]
pub(crate) trait DirLister: Send + Sync {
    async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>>;
}

#[async_trait]
impl DirLister for Arc<dyn RemoteClient> {
    async fn list(&self, path: &RemotePath) -> nyx_core::Result<Vec<RemoteEntry>> {
        self.list_dir(path).await
    }
}

/// End the in-flight search (if any): flag the client walk to stop and abort the
/// task, which drops a server-side `find`'s channel and kills it.
pub(crate) fn abort_search(current: &mut Option<(Arc<AtomicBool>, tokio::task::JoinHandle<()>)>) {
    if let Some((flag, handle)) = current.take() {
        flag.store(true, Ordering::Relaxed);
        handle.abort();
    }
}

/// Run a tree search: try to offload it to the server (`find` over SSH `exec`),
/// and fall back to the client-side walk when the protocol/server can't - FTP, a
/// jailed sftp-only server, or a query with `size:`/`modified:` terms `find`
/// can't express here.
pub(crate) async fn run_tree_search(
    client: Arc<dyn RemoteClient>,
    root: RemotePath,
    query: Filter,
    token: u64,
    cancel: Arc<AtomicBool>,
    events: FuturesSender<Event>,
) {
    if let Some(predicates) = query.as_find_predicates() {
        // `Ok(None)` (unsupported) or `Err` (failed exec) → fall through to the
        // client walk; only a `Some(paths)` short-circuits.
        if let Ok(Some(paths)) = client
            .server_search(&root, &predicates, SEARCH_MAX_RESULTS)
            .await
        {
            emit_find_results(&events, token, &predicates, paths);
            return;
        }
    }
    run_search(&client, root, query, token, cancel, events).await;
}

/// Stream server-`find` matches to the UI. The paths carry no metadata, so each
/// entry is synthesized - kind from a `-type` predicate when present (else file),
/// size/mtime unknown (the UI renders those as "—").
pub(crate) fn emit_find_results(
    events: &FuturesSender<Event>,
    token: u64,
    predicates: &[nyx_core::FindPredicate],
    paths: Vec<RemotePath>,
) {
    use nyx_core::FindPredicate;
    let kind = predicates
        .iter()
        .find_map(|p| match p {
            FindPredicate::Kind(k) => Some(*k),
            _ => None,
        })
        .unwrap_or(EntryKind::File);
    let truncated = paths.len() >= SEARCH_MAX_RESULTS;

    let mut remaining: Vec<SearchHit> = paths
        .into_iter()
        .map(|path| {
            let name = path.file_name().unwrap_or_default().to_string();
            SearchHit {
                entry: RemoteEntry {
                    name,
                    size: 0,
                    kind,
                    modified: None,
                    permissions: Permissions::from_mode(0),
                },
                path,
            }
        })
        .collect();

    // Stream in the same batch size the walk uses, ending with a terminal `done`
    // batch (empty when there were no matches at all).
    loop {
        let take = remaining.len().min(SEARCH_BATCH);
        let chunk: Vec<SearchHit> = remaining.drain(..take).collect();
        let done = remaining.is_empty();
        let _ = events.unbounded_send(Event::SearchResult {
            token,
            hits: chunk,
            done,
            truncated: done && truncated,
        });
        if done {
            break;
        }
    }
}

/// Breadth-first walk of `root`, streaming matched entries back in batches.
///
/// Up to [`SEARCH_CONCURRENCY`] directory listings run **in flight at once** -
/// the dominant cost of a deep search is sequential round-trips, and SFTP
/// multiplexes requests over its one connection, so concurrency is the big win.
/// Bounded by [`SEARCH_MAX_DEPTH`] (also the symlink-loop backstop) and
/// [`SEARCH_MAX_RESULTS`]. A directory that fails to list (permission denied, a
/// vanished path) is skipped, not fatal. A completed directory's matches are
/// flushed right away so results appear as they're found, not only at the end.
/// The walk checks `cancel` each turn and bails when a newer search supersedes
/// it - the UI ignores that token anyway, so no terminal batch is owed.
pub(crate) async fn run_search(
    client: &(impl DirLister + ?Sized),
    root: RemotePath,
    query: Filter,
    token: u64,
    cancel: Arc<AtomicBool>,
    events: FuturesSender<Event>,
) {
    let now = SystemTime::now();
    let mut frontier: VecDeque<(RemotePath, u32)> = VecDeque::new();
    frontier.push_back((root, 0));
    let mut inflight = FuturesUnordered::new();
    let mut batch: Vec<SearchHit> = Vec::new();
    let mut found = 0usize;
    let mut truncated = false;

    'walk: loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        // Keep the connection busy: top in-flight listings up to the cap.
        while inflight.len() < SEARCH_CONCURRENCY {
            let Some((dir, depth)) = frontier.pop_front() else {
                break;
            };
            inflight.push(async move {
                let entries = client.list(&dir).await;
                (dir, depth, entries)
            });
        }
        // Frontier drained and nothing in flight → the walk is done.
        let Some((dir, depth, result)) = inflight.next().await else {
            break;
        };
        let Ok(entries) = result else {
            continue; // unreadable directory: skip, keep searching
        };
        for entry in entries {
            let name_lower = entry.name.to_lowercase();
            let path = dir.join(&entry.name);
            if entry.is_dir() && depth < SEARCH_MAX_DEPTH {
                frontier.push_back((path.clone(), depth + 1));
            }
            if query.matches(&entry, &name_lower, now) {
                batch.push(SearchHit { path, entry });
                found += 1;
                if batch.len() >= SEARCH_BATCH {
                    flush_hits(&events, token, &mut batch, false, false);
                }
                if found >= SEARCH_MAX_RESULTS {
                    truncated = true;
                    break 'walk;
                }
            }
        }
        // Stream this directory's matches now, rather than waiting for the cap.
        if !batch.is_empty() {
            flush_hits(&events, token, &mut batch, false, false);
        }
    }
    flush_hits(&events, token, &mut batch, true, truncated);
}

/// Send one [`Event::SearchResult`] batch, draining `batch`.
pub(crate) fn flush_hits(
    events: &FuturesSender<Event>,
    token: u64,
    batch: &mut Vec<SearchHit>,
    done: bool,
    truncated: bool,
) {
    let hits = std::mem::take(batch);
    let _ = events.unbounded_send(Event::SearchResult {
        token,
        hits,
        done,
        truncated,
    });
}
