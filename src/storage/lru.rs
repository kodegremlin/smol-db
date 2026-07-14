use std::collections::HashMap;

use crate::storage::page::PageId;

/// Represents a single entry in our arena-backed doubly-linked list.
#[derive(Debug)]
struct LruNode {
    prev: Option<usize>,
    page_id: PageId,
    next: Option<usize>,
}

/// Tracks page usage history to determine eviction candidates.
/// [MRU / HEAD]                              [LRU / TAIL]
///      <- prev                next ->
/// [30] <----------> [20] <----------> [10]
#[derive(Debug)]
pub struct LruReplacer {
    /// Maps a [PageId] to its index within the `arena` vector.
    pages: HashMap<PageId, usize>,

    /// The contiguous memory block storing our linked-list nodes.
    arena: Vec<LruNode>,

    /// Index of the most recently used node.
    head: Option<usize>,

    /// Index of the least recently used node (the eviction
    /// candidate).
    tail: Option<usize>,

    /// Maximum number of pages the replacer is allowed to track.
    capacity: usize,

    /// Tracks indices of nodes that have been removed, allowing
    /// arena slot reuse.
    free_list: Vec<usize>,
}

impl LruReplacer {
    /// Initializes an empty `LruReplacer` with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            pages: HashMap::with_capacity(capacity),
            arena: Vec::with_capacity(capacity),
            head: None,
            tail: None,
            capacity,
            free_list: Vec::with_capacity(capacity),
        }
    }

    /// Records that a page was accessed, making it the most recently used.
    /// If the page is new, a new node is pushed into the arena, at either
    /// a free_idx or at the last index.
    ///
    /// Panics if the replacer exceeds capacity without the [BufferPool]
    /// evicting first.
    pub fn record_access(&mut self, page_id: PageId) {
        if let Some(&idx) = self.pages.get(&page_id) {
            self.unlink(idx);
            return self.insert_head(idx);
        }
        assert!(
            self.pages.len() < self.capacity,
            "Buffer Pool exceeded capacity! Must call evict() before adding new pages."
        );
        // Reuse a free slot if available, otherwise append to the arena.
        let new_idx = if let Some(free_idx) = self.free_list.pop() {
            self.arena[free_idx] = LruNode {
                prev: None,
                page_id,
                next: None,
            };
            free_idx
        } else {
            let new_idx = self.arena.len();
            let node = LruNode {
                prev: None,
                page_id,
                next: None,
            };
            self.arena.push(node);
            new_idx
        };
        self.pages.insert(page_id, new_idx);
        self.insert_head(new_idx);
    }

    /// Explicitly removes a page from tracking.
    pub fn remove(&mut self, page_id: PageId) {
        if let Some(idx) = self.pages.remove(&page_id) {
            self.unlink(idx);
            self.free_list.push(idx);
        }
    }

    /// Inserts a node at the head (most recently used) position.
    fn insert_head(&mut self, idx: usize) {
        self.arena[idx].prev = None;
        self.arena[idx].next = self.head;

        if let Some(h_idx) = self.head {
            self.arena[h_idx].prev = Some(idx);
        } else {
            // If there is no head, the list was empty; this node is also the tail.
            self.tail = Some(idx);
        }
        self.head = Some(idx)
    }

    /// Unlinks a node from its current position in the linked list.
    fn unlink(&mut self, idx: usize) {
        let prev_idx = self.arena[idx].prev;
        let next_idx = self.arena[idx].next;

        if let Some(p_idx) = prev_idx {
            self.arena[p_idx].next = next_idx;
        } else {
            // If there is no prev node, this node was the head.
            self.head = next_idx;
        }
        if let Some(n_idx) = next_idx {
            self.arena[n_idx].next = prev_idx;
        } else {
            // If there is no next node, this node was the tail.
            self.tail = prev_idx;
        }
    }

    /// Removes and returns the least recently used `PageId` from the replacer.
    /// The caller, [BufferPool] is responsible for safely flushing dirty data.
    /// Returns `None` if the replacer is empty.
    pub fn evict(&mut self) -> Option<PageId> {
        let t_idx = self.tail?;
        let page_id = self.arena[t_idx].page_id;

        self.unlink(t_idx);
        self.pages.remove(&page_id);
        self.free_list.push(t_idx);

        Some(page_id)
    }
}
