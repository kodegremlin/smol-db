use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use crate::{
    error::DbError,
    storage::{
        lru::LruReplacer,
        page::{BTreeNode, DiskManager, Page, PageId},
    },
};

/// Decouples the memory manager from the physical Wal implementation.
pub trait WalFlusher: Send + Sync {
    /// Forces the Wal manager to synchronously write and fsync all
    /// log records up-to and including the specified lsn, out to
    /// the non-volatile disk.
    fn flush_upto(&self, lsn: u64) -> Result<(), DbError>;
}

/// A thread-safe referance to a cached page. The `Arc` provides lifecycle
/// tracking (pinning), and the `RwLock` provides per page "latching".
pub type Frame = Arc<RwLock<BTreeNode>>;

/// Handles memory caching, page fetching, and eviction.
pub struct BufferPool {
    disk_manager: DiskManager,
    replacer: LruReplacer,
    page_table: HashMap<PageId, Frame>,
    capacity: usize,
    wal_flusher: Option<Arc<dyn WalFlusher>>,
}

impl BufferPool {
    /// Creates a new BufferPool with a specified capacity.
    pub fn new(
        disk_manager: DiskManager,
        capacity: usize,
        wal_flusher: Option<Arc<dyn WalFlusher>>,
    ) -> Self {
        Self {
            disk_manager,
            replacer: LruReplacer::new(capacity),
            page_table: HashMap::with_capacity(capacity),
            capacity,
            wal_flusher,
        }
    }

    /// Fetches a page from the buffer pool. If it's a cache miss, it reads
    /// from disk, potentially evicting an old page.
    pub fn fetch_page(&mut self, page_id: PageId) -> Result<Frame, DbError> {
        if let Some(frame) = self.page_table.get(&page_id) {
            self.replacer.record_access(page_id);
            return Ok(frame.clone());
        }

        // cache miss: we'll have to load the page from disk.
        if self.page_table.len() >= self.capacity {
            self.evict_page()?;
        }

        // Read physical bytes from disk and decode it into a in-memory BTreeNode.
        let raw_page = self.disk_manager.read_page(&page_id)?;
        let node = BTreeNode::decode(&raw_page)?;

        let frame = Arc::new(RwLock::new(node));

        self.page_table.insert(page_id, frame.clone());
        self.replacer.record_access(page_id);

        Ok(frame)
    }

    /// Allocates a completely new page via the `DiskManager` and adds it to the pool.
    pub fn new_page(&mut self, is_leaf: bool) -> Result<(PageId, Frame), DbError> {
        if self.page_table.len() >= self.capacity {
            self.evict_page()?;
        }
        let page_id = self.disk_manager.allocate_page();

        let node = BTreeNode::new_empty(page_id, is_leaf);
        let frame = Arc::new(RwLock::new(node));

        self.page_table.insert(page_id, frame.clone());
        self.replacer.record_access(page_id);

        Ok((page_id, frame))
    }

    /// Flushes a specific page to disk if it is dirty.
    pub fn flush_page(&mut self, page_id: PageId) -> Result<(), DbError> {
        if let Some(frame) = self.page_table.get(&page_id) {
            let mut node_guard = frame
                .write()
                .map_err(|_| DbError::CorruptPage("Poisoned lock".into()))?;

            if node_guard.is_dirty() {
                let mut raw_page = Page::new();
                node_guard.encode(&mut raw_page)?;
                self.disk_manager.write_page(page_id, &raw_page)?;
                node_guard.clear_dirty();
            }
        }
        Ok(())
    }

    /// Flushes all dirty pages to disk.
    pub fn flush_all_pages(&mut self) -> Result<(), DbError> {
        let page_ids: Vec<PageId> = self.page_table.keys().copied().collect();
        for page_id in page_ids {
            self.flush_page(page_id)?;
        }
        self.disk_manager.save_header()?;
        Ok(())
    }

    /// Find a page that can be evicted, flush it if dirty, and remove it from
    /// memory.
    fn evict_page(&mut self) -> Result<(), DbError> {
        let evict_id = self
            .replacer
            .evict_if(|page_id| match self.page_table.get(page_id) {
                Some(frame) => Arc::strong_count(frame) == 1,
                None => {
                    panic!(
                        "LruReplacer contains PageId({:?}); should also be present in page_table",
                        page_id
                    );
                }
            })
            .ok_or(DbError::LruEviction)?;

        self.flush_page(evict_id)?;
        self.page_table.remove(&evict_id);
        Ok(())
    }
}
