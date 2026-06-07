//! Protocol-agnostic transfer + traversal helpers shared by every
//! [`RemoteClient`](crate::RemoteClient) implementation.
//!
//! These are lifted out of the SFTP client so FTP/FTPS feed the same
//! [`TransferProgress`] contract (chunk size, cancel-between-chunks) and reuse
//! the same async-recursion-free walk/removal planning.

use std::future::Future;

use nyx_core::{EntryKind, NyxError, Result, TransferProgress};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::{DirWalk, WalkItem};

/// The copy-loop chunk size (64 KiB).
pub(crate) const COPY_CHUNK: usize = 64 * 1024;

/// Copy `reader` → `writer` in 64 KiB chunks, bumping `progress` per chunk and
/// checking for a requested cancel between chunks.
///
/// Both halves are driven through tokio's `AsyncRead`/`AsyncWrite`, which surface
/// `std::io::Error` regardless of which side errors — hence the single
/// [`map_io_err`]. A cancel short-circuits with [`NyxError::Cancelled`]; the
/// caller (service) does any partial-file cleanup.
pub(crate) async fn copy_counting<R, W>(
    reader: &mut R,
    writer: &mut W,
    progress: &TransferProgress,
) -> Result<()>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let mut buf = vec![0u8; COPY_CHUNK];
    loop {
        if progress.is_cancelled() {
            return Err(NyxError::Cancelled);
        }
        let n = reader.read(&mut buf).await.map_err(map_io_err)?;
        if n == 0 {
            break;
        }
        writer.write_all(&buf[..n]).await.map_err(map_io_err)?;
        progress.add(n as u64);
    }
    Ok(())
}

/// Map a **local** filesystem / transfer-copy error to [`NyxError`]. Used for the
/// local half of a transfer; paths aren't secrets, but the message stays coarse
/// (an OS error string, never a credential).
pub(crate) fn map_io_err(err: std::io::Error) -> NyxError {
    NyxError::Io(err.to_string())
}

/// One step of a recursive removal, in the order it must be performed: a
/// directory only ever appears **after** all of its descendants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RemoveOp {
    /// Delete a file (or symlink) at this absolute path.
    File(String),
    /// Delete a now-empty directory at this absolute path.
    Dir(String),
}

/// Plan the depth-first removal of `root` without async recursion.
///
/// A file target yields a single [`RemoveOp::File`]. A directory target is
/// walked with an explicit work-stack: each directory is visited twice — once to
/// list its children (pushing sub-directories back on the stack and emitting its
/// files), and once (after its children) to emit the directory's own
/// [`RemoveOp::Dir`]. `list_dir` yields each directory's `(path, is_dir)`
/// children. The result is post-order, so applying it in sequence removes the
/// whole tree leaf-first.
pub(crate) async fn plan_removal<F, Fut>(
    root: &str,
    root_is_dir: bool,
    mut list_dir: F,
) -> Result<Vec<RemoveOp>>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<(String, bool)>>>,
{
    if !root_is_dir {
        return Ok(vec![RemoveOp::File(root.to_string())]);
    }
    let mut ops = Vec::new();
    // (path, expanded): an unexpanded directory still needs listing; an expanded
    // one has had its children queued and is ready to be removed.
    let mut stack: Vec<(String, bool)> = vec![(root.to_string(), false)];
    while let Some((path, expanded)) = stack.pop() {
        if expanded {
            ops.push(RemoveOp::Dir(path));
            continue;
        }
        let children = list_dir(path.clone()).await?;
        // Re-push this directory below its children so it is removed last.
        stack.push((path, true));
        for (child, is_dir) in children {
            if is_dir {
                stack.push((child, false));
            } else {
                ops.push(RemoveOp::File(child));
            }
        }
    }
    Ok(ops)
}

/// Plan a recursive directory walk without async recursion, mirroring
/// [`plan_removal`] but **pre-order** (parent before children) and producing
/// copy work items rather than deletes.
///
/// `list_dir` yields each visited directory's `(name, kind, size)` children. A
/// directory's [`WalkItem`] is emitted while listing its *parent*, before the
/// directory itself is listed — so the result orders every parent ahead of its
/// descendants. Symlinks (and other non-file/non-dir entries) are skipped and
/// tallied; we don't follow links in v1.
pub(crate) async fn plan_walk<F, Fut>(root: &str, mut list_dir: F) -> Result<DirWalk>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Result<Vec<(String, EntryKind, u64)>>>,
{
    let mut walk = DirWalk::default();
    // (absolute dir path, components relative to the walk root) still to list.
    let mut stack: Vec<(String, Vec<String>)> = vec![(root.to_string(), Vec::new())];
    while let Some((dir, rel)) = stack.pop() {
        for (name, kind, size) in list_dir(dir.clone()).await? {
            let mut child_rel = rel.clone();
            child_rel.push(name.clone());
            let child_abs = format!("{dir}/{name}");
            match kind {
                EntryKind::Directory => {
                    walk.items.push(WalkItem {
                        rel: child_rel.clone(),
                        is_dir: true,
                        size: 0,
                    });
                    stack.push((child_abs, child_rel));
                }
                EntryKind::File => {
                    walk.total_bytes += size;
                    walk.items.push(WalkItem {
                        rel: child_rel,
                        is_dir: false,
                        size,
                    });
                }
                // Links and special files (sockets, devices, …) aren't copyable
                // byte streams — skip and tally, never follow.
                EntryKind::Symlink | EntryKind::Other => walk.skipped += 1,
            }
        }
    }
    Ok(walk)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drive an async future to completion on a minimal current-thread runtime
    /// (the traversal is async but server-free in the test).
    fn block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(fut)
    }

    #[test]
    fn walk_is_pre_order_with_totals_and_skips() {
        use std::collections::HashMap;

        // /root
        //   a.txt        (10)
        //   link  -> …   (symlink, skipped)
        //   sub/
        //     c.txt      (3)
        //     deep/
        //       d.txt    (4)
        let tree: HashMap<&str, Vec<(&str, EntryKind, u64)>> = HashMap::from([
            (
                "/root",
                vec![
                    ("a.txt", EntryKind::File, 10),
                    ("link", EntryKind::Symlink, 0),
                    ("sub", EntryKind::Directory, 0),
                ],
            ),
            (
                "/root/sub",
                vec![
                    ("c.txt", EntryKind::File, 3),
                    ("deep", EntryKind::Directory, 0),
                ],
            ),
            ("/root/sub/deep", vec![("d.txt", EntryKind::File, 4)]),
        ]);

        let walk = block_on(plan_walk("/root", |dir| {
            let tree = &tree;
            async move {
                Ok(tree
                    .get(dir.as_str())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(n, k, s)| (n.to_string(), k, s))
                    .collect())
            }
        }))
        .unwrap();

        assert_eq!(walk.total_bytes, 17);
        assert_eq!(walk.skipped, 1);
        // Every directory precedes anything beneath it.
        let pos = |rel: &[&str]| {
            walk.items
                .iter()
                .position(|i| i.rel == rel)
                .unwrap_or_else(|| panic!("missing {rel:?}"))
        };
        assert!(pos(&["sub"]) < pos(&["sub", "c.txt"]));
        assert!(pos(&["sub"]) < pos(&["sub", "deep"]));
        assert!(pos(&["sub", "deep"]) < pos(&["sub", "deep", "d.txt"]));
        // The skipped symlink is not an item.
        assert!(!walk.items.iter().any(|i| i.rel == ["link"]));
        // An empty directory still emits its own item (so it gets created).
        assert!(walk
            .items
            .iter()
            .any(|i| i.rel == ["sub", "deep"] && i.is_dir));
    }

    #[test]
    fn removal_of_a_file_is_a_single_op() {
        let ops = block_on(plan_removal("/srv/report.pdf", false, |_| async {
            unreachable!("a file target is never listed")
        }))
        .unwrap();
        assert_eq!(ops, vec![RemoveOp::File("/srv/report.pdf".into())]);
    }

    #[test]
    fn removal_of_a_tree_is_post_order() {
        use std::collections::HashMap;

        // /root
        //   a.txt
        //   sub/
        //     c.txt
        //     deep/
        //       d.txt
        //   b.txt
        let tree: HashMap<&str, Vec<(&str, bool)>> = HashMap::from([
            (
                "/root",
                vec![
                    ("/root/a.txt", false),
                    ("/root/sub", true),
                    ("/root/b.txt", false),
                ],
            ),
            (
                "/root/sub",
                vec![("/root/sub/c.txt", false), ("/root/sub/deep", true)],
            ),
            ("/root/sub/deep", vec![("/root/sub/deep/d.txt", false)]),
        ]);

        let ops = block_on(plan_removal("/root", true, |dir| {
            let tree = &tree;
            async move {
                Ok(tree
                    .get(dir.as_str())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(p, is_dir)| (p.to_string(), is_dir))
                    .collect())
            }
        }))
        .unwrap();

        // Every file before its parent dir; every dir after all its descendants.
        assert_eq!(
            ops,
            vec![
                RemoveOp::File("/root/a.txt".into()),
                RemoveOp::File("/root/b.txt".into()),
                RemoveOp::File("/root/sub/c.txt".into()),
                RemoveOp::File("/root/sub/deep/d.txt".into()),
                RemoveOp::Dir("/root/sub/deep".into()),
                RemoveOp::Dir("/root/sub".into()),
                RemoveOp::Dir("/root".into()),
            ]
        );
    }
}
