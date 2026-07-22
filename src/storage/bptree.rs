use crate::{
    error::DbError,
    storage::{
        buffer_pool::BufferPool,
        page::{BTreeNode, PageId},
    },
};

/// Represents a B+ Tree access method backed by slotted page `LeafNode`s.
pub struct BpTree<'a> {
    /// Mutable reference to the centralized Buffer Pool conveyor belt.
    buffer_pool: &'a mut BufferPool,

    /// The physical PageId of the current root node.
    root_page_id: PageId,
}

impl<'a> BpTree<'a> {
    /// Constructs a initialized B+Tree with the given `BufferPool` and `PageId`.
    pub fn new(pool: &'a mut BufferPool, root_page_id: PageId) -> Self {
        Self {
            buffer_pool: pool,
            root_page_id,
        }
    }

    /// Returns the active root PageId.
    pub fn get_root_id(&self) -> PageId {
        self.root_page_id
    }

    /// Executes a binary search to retreive a record payload by the given primary
    /// key a.k.a. row_id.
    ///
    /// Returns an owned copy of the payload if found and not logically deleted.
    pub fn find_record(&mut self, target_key: u64) -> Result<Option<Vec<u8>>, DbError> {
        let mut curr_page_id = self.root_page_id;
        loop {
            let frame = self.buffer_pool.fetch_page(curr_page_id)?;
            let node_gurad = frame.read();
            match &*node_gurad {
                BTreeNode::Internal(node) => {
                    let next_page_id = node.route_key(target_key)?;
                    curr_page_id = next_page_id;
                }
                BTreeNode::Leaf(node) => return Ok(node.get_record(target_key)),
            }
        }
    }
}
