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

/// A thread-safe referance to a cached page. The `Arc` provides lifecycle
/// tracking (pinning), and the `RwLock` provides per page "latching".
pub type PageRef = Arc<RwLock<BTreeNode>>;

/// Handles memory caching, page fetching, and eviction.
pub struct BufferPool {
    disk_manager: DiskManager,
    replacer: LruReplacer,
    page_table: HashMap<PageId, PageRef>,
    capacity: usize,
}

impl BufferPool {
    /// Creates a new BufferPool with a specified capacity.
    pub fn new(disk_manager: DiskManager, capacity: usize) -> Self {
        Self {
            disk_manager,
            replacer: LruReplacer::new(capacity),
            page_table: HashMap::with_capacity(capacity),
            capacity,
        }
    }

    /// Fetches a page from the buffer pool. If it's a cache miss, it reads
    /// from disk, potentially evicting an old page.
    pub fn fetch_page(&mut self, page_id: PageId) -> Result<PageRef, DbError> {
        if let Some(page_ref) = self.page_table.get(&page_id) {
            self.replacer.record_access(page_id);
            return Ok(page_ref.clone());
        }

        // cache miss: we'll have to load the page from disk.
        if self.page_table.len() >= self.capacity {
            self.evict_page()?;
        }

        // Read physical bytes from disk and decode it into a in-memory BTreeNode.
        let raw_page = self.disk_manager.read_page(&page_id)?;
        let node = BTreeNode::decode(&raw_page)?;

        let page_ref = Arc::new(RwLock::new(node));

        self.page_table.insert(page_id, page_ref.clone());
        self.replacer.record_access(page_id);

        Ok(page_ref)
    }

    /// Allocates a completely new page via the `DiskManager` and adds it to the pool.
    pub fn new_page(&mut self, is_leaf: bool) -> Result<(PageId, PageRef), DbError> {
        if self.page_table.len() >= self.capacity {
            self.evict_page()?;
        }
        let page_id = self.disk_manager.allocate_page();

        let node = BTreeNode::new_empty(page_id, is_leaf);
        let page_ref = Arc::new(RwLock::new(node));

        self.page_table.insert(page_id, page_ref.clone());
        self.replacer.record_access(page_id);

        Ok((page_id, page_ref))
    }

    /// Flushes a specific page to disk if it is dirty.
    pub fn flush_page(&mut self, page_id: PageId) -> Result<(), DbError> {
        if let Some(page_ref) = self.page_table.get(&page_id) {
            let mut node = page_ref
                .write()
                .map_err(|_| DbError::CorruptPage("Poisoned lock".into()))?;

            if node.is_dirty() {
                let mut raw_page = Page::new();
                node.encode(&mut raw_page)?;
                self.disk_manager.write_page(page_id, &raw_page)?;
                node.clear_dirty();
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
        // Attempt to find an unpinned victim page.
        // If strong_count > 1, the storage engine is actively using it (pinned).
        let mut remove_id = None;

        for _ in 0..self.replacer.size() {
            if let Some(candidate_id) = self.replacer.evict() {
                let Some(page_ref) = self.page_table.get(&candidate_id) else {
                    panic!("page for candidate_id={} should be present", candidate_id.0);
                };
                if Arc::strong_count(page_ref) == 1 {
                    remove_id = Some(candidate_id);
                    break;
                }
                // If it was pinned, put it back as recently used and try
                // the next oldest.
                self.replacer.record_access(candidate_id);
            }
        }
        // The error should almost never be the case.
        let evict_id = remove_id.ok_or(DbError::PageFull)?;

        self.flush_page(evict_id)?;
        self.page_table.remove(&evict_id);
        Ok(())
    }
}
