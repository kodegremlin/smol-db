use std::{cmp, path::Path};

use crate::{
    error::DbError,
    storage::{
        buffer_pool::BufferPool,
        page::BTreeNode,
        wal::{WalBatch, WalEntry, WalManager, WalOp},
    },
};

/// Handles database startup recovery by replaying historical Wals against in-memory
/// pages in the [BufferPool].
///
/// Enforces ARIES redo idempotency: physical updates are ignored if the target
/// disk page has already recorded an Lsn equal to or greater than the log record.
pub struct RecoveryEngine;

impl RecoveryEngine {
    /// Bootstraps database recovery on startup.
    /// Returns the highest observed Lsn so the storage engine can initialize
    /// its runtime counter.
    pub fn init<P>(path: P, buf_pool: &mut BufferPool) -> Result<u64, DbError>
    where
        P: AsRef<Path>,
    {
        let mut wal = WalManager::open(path, false)?;
        let batch = wal.read_batch()?;

        // If the wal is completely empty, either the database shut down cleanly
        // or is brand new.
        if batch.is_empty() {
            return Ok(wal.flushed_lsn());
        }
        // Replay the log records chronologically against the buffer pool.
        let max_lsn = Self::replay(buf_pool, &batch)?;

        // Wipe the obsolete log.
        wal.truncate()?;
        wal.sync()?;

        Ok(max_lsn)
    }

    /// Replays a sequence of Wal entries against the Buffer Pool to reconstruct
    /// lost memory state.
    pub fn replay(buf_pool: &mut BufferPool, batch: &WalBatch) -> Result<u64, DbError> {
        let mut max_observed_lsn = 0;

        for entry in batch {
            max_observed_lsn = cmp::max(max_observed_lsn, entry.lsn);
            {
                // Acquire RAII write guard over the page buffer.
                let frame = buf_pool.fetch_page(entry.page_id)?;
                let mut node = frame
                    .write()
                    .map_err(|_| DbError::CorruptPage("poisoned lock".into()))?;

                // If the physical page on disk already bears an Lsn >= log's
                // lsn this update was already written to disk.
                if entry.lsn <= node.get_last_lsn() {
                    continue;
                }
                Self::apply_entry(&mut node, entry)?;
                node.mark_dirty(entry.lsn);
            }
        }
        buf_pool.flush_all_pages()?;
        Ok(max_observed_lsn)
    }

    /// Applies a single physical Wal operation to an in-memory page.
    fn apply_entry(node: &mut BTreeNode, entry: &WalEntry) -> Result<(), DbError> {
        /* NOTE: we could design the Record data structure to avoid cloning
        potentially a large payload, however that could complicate the
        design around complicated lifetimes and right now we'll keep it
        simple and in future when we add transaction and shit, we'll see.*/
        match entry.opcode {
            WalOp::Insert => node.insert_record(entry.row_id, entry.payload.clone()),
            WalOp::Update => node.update_record(entry.row_id, entry.payload.clone()),
            WalOp::Delete => node.delete_record(entry.row_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self},
        path::PathBuf,
    };

    use crate::storage::{
        buffer_pool::BufferPool,
        page::{BTreeNode, DiskManager, LeafNode, Page, PageId},
        recovery::RecoveryEngine,
        wal::{WalEntry, WalManager, WalOp},
    };

    /// Helper to bootstrap an isolated Buffer Pool in a temporary directory.
    fn setup_test_pool(name: &str) -> (BufferPool, PageId, PathBuf) {
        let mut path = PathBuf::from("/Volumes/External T7/");
        path.push(format!("test.tbl{}", name));

        let _ = fs::remove_file(&path);
        let disk_manager = DiskManager::open(&path).expect("opening Disk Manager failed");

        let empty_page = BTreeNode::Leaf(LeafNode::default());
        let mut buffer = Page::new();

        empty_page
            .encode(&mut buffer)
            .expect("encoding empty page failed");

        disk_manager
            .write_page(PageId(0), &buffer)
            .expect("writing buffer to page failed");

        let pool = BufferPool::new(disk_manager, 10, None);
        (pool, PageId(0), path)
    }

    #[test]
    fn test_aries_redo_idempotency_skips_flushed_pages() {
        let (mut pool, page_id, path) = setup_test_pool("");
        {
            let frame = pool.fetch_page(page_id).unwrap();
            let mut node = frame.write().unwrap();
            node.insert_record(1, vec![10, 20]).unwrap();
            node.mark_dirty(100);
        }
        pool.flush_all_pages().unwrap();

        let batch = vec![
            WalEntry::new(WalOp::Insert, 50, page_id, 1, vec![99, 99]), // old: should be skipped
            WalEntry::new(WalOp::Insert, 100, page_id, 1, vec![10, 20]), // same: should be skipped
            WalEntry::new(WalOp::Update, 150, page_id, 1, vec![88, 88]), // new: should be applied
        ];
        let max_lsn = RecoveryEngine::replay(&mut pool, &batch).expect("replay failed");
        assert_eq!(max_lsn, 150);

        let frame = pool.fetch_page(page_id).unwrap();
        let node = frame.write().unwrap();
        assert_eq!(node.get_last_lsn(), 150);
        dbg!(&node);

        // If the Lsn has not been skipped, the value would be [88, 88]
        match &*node {
            BTreeNode::Leaf(node) => {
                assert_eq!(node.records.len(), 1);
                assert_eq!(node.records[0].data, vec![88, 88])
            }
            _ => panic!("expected leaf node"),
        }
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_init_storage_recovers_and_truncates_log() {
        let (mut pool, page_id, db_path) = setup_test_pool("");
        let path = PathBuf::from("/Volumes/External T7/test.wal");

        // Write a persistent log batch to disk via WalManager
        {
            let mut wal = WalManager::open(&path, true).unwrap();
            let batch = vec![WalEntry::new(
                WalOp::Insert,
                10,
                page_id,
                42,
                vec![1, 2, 3, 4],
            )];
            wal.write_batch(&batch).unwrap();
        }
        assert!(fs::metadata(&path).unwrap().len() > 0);

        let recovered_lsn = RecoveryEngine::init(&path, &mut pool).unwrap();
        assert_eq!(recovered_lsn, 10);

        // Verify the page was reconstructed in RAM
        let frame = pool.fetch_page(page_id).unwrap();
        let node = frame.read().unwrap();
        assert_eq!(node.get_last_lsn(), 10);

        // Verify Log Reclamation (WAL must be truncated to 0 bytes!)
        let post_recovery_wal_size = fs::metadata(&path).unwrap().len();
        assert_eq!(
            post_recovery_wal_size, 0,
            "WAL file was not truncated after checkpoint!"
        );
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(db_path);
    }
}
