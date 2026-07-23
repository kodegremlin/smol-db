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

/// Communicates a structural leaf split towards the parent internal routing nodes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitResult {
    /// The primary key that divides the left and right leaf pages.
    pub promoted_row_id: u64,

    /// The physical `PageId` of the newly allocated right sibling leaf.
    pub new_page_id: PageId,
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

    /// Inserts a key-value payload directly into a target leaf page. If physical
    /// capacity is exceeded, allocates a sibling page from the `BufferPool`,
    /// splits the current leaf into half writing the other half of the data into
    /// the new sibling leaf, and then compact the old page.
    ///
    /// Returns the `promoted_row_id` and the `new_page_id` if split happens as
    /// `Some(SplitResult)` or `None` if data was written to the current page only.
    pub fn insert_leaf(
        &mut self,
        leaf_page_id: PageId,
        row_id: u64,
        payload: Vec<u8>,
        lsn: u64,
    ) -> Result<Option<SplitResult>, DbError> {
        let leaf_frame = self.buffer_pool.fetch_page(leaf_page_id)?;
        let mut node_guard = leaf_frame.write();

        let left_leaf = match &mut *node_guard {
            BTreeNode::Leaf(node) => node,
            BTreeNode::Internal(_) => {
                return Err(DbError::CorruptPage(
                    "attempted to insert data on a internal routing node".into(),
                ));
            }
        };
        match left_leaf.insert_record(row_id, payload.clone()) {
            Ok(()) => {
                node_guard.mark_dirty(lsn);
                return Ok(None);
            }
            Err(DbError::DuplicateKey(key)) => return Err(DbError::DuplicateKey(key)),
            Err(DbError::PageFull) => {
                // physical page capacity exceeded, we'll split the page below.
            }
            Err(err) => return Err(err),
        }
        let (new_leaf_id, new_leaf_frame) = self.buffer_pool.new_page(true)?;
        let mut new_node_guard = new_leaf_frame.write();

        let right_leaf = match &mut *new_node_guard {
            BTreeNode::Leaf(node) => node,
            _ => unreachable!("new_page(true) must return a LeafNode"),
        };
        let promoted_row_id = left_leaf.split(right_leaf)?;

        if row_id < promoted_row_id {
            left_leaf.insert_record(row_id, payload)?;
        } else {
            right_leaf.insert_record(row_id, payload)?;
        }
        // store the previous pointer state
        let old_next_id = left_leaf.next_page_id;
        let old_has_next = left_leaf.has_next;

        // wire the new right sibling node between the left_leaf and old right.
        right_leaf.has_prev = true;
        right_leaf.prev_page_id = leaf_page_id.into();

        right_leaf.has_next = old_has_next;
        right_leaf.next_page_id = old_next_id;

        // wire the left_leaf to point towards the right_leaf.
        left_leaf.has_next = true;
        left_leaf.next_page_id = new_leaf_id.into();

        // If an old right sibling existed, fetch it and connects its backward
        // pointer.
        if old_has_next {
            let old_right_frame = self.buffer_pool.fetch_page(PageId(old_next_id))?;
            let mut old_right_guard = old_right_frame.write();

            if let BTreeNode::Leaf(ref mut old_right_leaf) = *old_right_guard {
                old_right_leaf.has_prev = true;
                old_right_leaf.prev_page_id = new_leaf_id.into();
                old_right_guard.mark_dirty(lsn);
            }
        }
        node_guard.mark_dirty(lsn);
        new_node_guard.mark_dirty(lsn);

        Ok(Some(SplitResult {
            promoted_row_id,
            new_page_id: new_leaf_id,
        }))
    }

    /// Executes a binary search to retreive a record payload by the given primary
    /// key a.k.a. row_id.
    ///
    /// Returns an owned copy of the payload if found and not logically deleted.
    pub fn find_record(&mut self, row_id: u64) -> Result<Option<Vec<u8>>, DbError> {
        let mut curr_page_id = self.root_page_id;
        loop {
            let frame = self.buffer_pool.fetch_page(curr_page_id)?;
            let node_gurad = frame.read();
            match &*node_gurad {
                BTreeNode::Internal(node) => {
                    let next_page_id = node.route_key(row_id)?;
                    curr_page_id = next_page_id;
                }
                BTreeNode::Leaf(node) => return Ok(node.get_record(row_id)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self},
        path::PathBuf,
    };

    use crate::{
        error::DbError,
        storage::{
            bptree::BpTree,
            buffer_pool::BufferPool,
            page::{BTreeNode, DiskManager},
        },
    };

    fn setup_pool(name: &str) -> (BufferPool, PathBuf) {
        let mut path = PathBuf::from("/Volumes/External T7/");
        path.push(format!("test_{}.tbl", name));
        let _ = fs::remove_file(&path);
        let disk_manager = DiskManager::open(&path).expect("opening DiskManager failed");
        let pool = BufferPool::new(disk_manager, 20, None);
        (pool, path)
    }

    #[test]
    fn test_insert_leaf_duplicate_rejection() {
        let (mut pool, db_path) = setup_pool("simple_insert");

        let (root_id, _) = pool.new_page(true).unwrap();
        let mut btree = BpTree::new(&mut pool, root_id);

        let result = btree
            .insert_leaf(root_id, 100, vec![1, 2, 3], 10)
            .unwrap();
        assert!(result.is_none());

        let result = btree
            .insert_leaf(root_id, 101, vec![4, 5, 6], 11)
            .unwrap();
        assert!(result.is_none());

        assert_eq!(btree.find_record(100).unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(btree.find_record(101).unwrap(), Some(vec![4, 5, 6]));

        let dup_result = btree.insert_leaf(root_id, 100, vec![10, 11, 13], 12);
        assert!(matches!(dup_result, Err(DbError::DuplicateKey(100))));

        let _ = fs::remove_file(db_path);
    }

    #[test]
    fn test_insert_leaf_split_and_sibling_chain() {
        let (mut pool, db_path) = setup_pool("split_chain");
        let (root_id, _) = pool.new_page(true).unwrap();
        let mut btree = BpTree::new(&mut pool, root_id);

        let large_payload = vec![0u8; 400];
        let mut split_res = None;

        for key in 1..=15 {
            if let Some(result) = btree
                .insert_leaf(root_id, key, large_payload.clone(), key)
                .unwrap()
            {
                split_res = Some(result);
                break;
            }
        }
        let split = split_res.expect("leaf failed to split exceeding 4KB capacity!");
        assert!(
            split.promoted_row_id >= 5,
            "row_id smaller than expected {}",
            split.promoted_row_id
        );
        assert_ne!(split.new_page_id, root_id);

        // verify sibling chaining & compaction

        let left_frame = pool.fetch_page(root_id).unwrap();
        let right_frame = pool.fetch_page(split.new_page_id).unwrap();

        let left_node = left_frame.read();
        let right_node = right_frame.read();

        match (&*left_node, &*right_node) {
            (BTreeNode::Leaf(left), BTreeNode::Leaf(right)) => {
                // verify left_page pointer to next node
                assert!(left.has_next);
                assert_eq!(left.next_page_id, split.new_page_id.into());

                // verify right_page pointer to prev node
                assert!(right.has_prev);
                assert_eq!(right.prev_page_id, root_id.into());

                assert_eq!(
                    right.records[right.slot_array[0] as usize].row_id,
                    split.promoted_row_id
                );
                assert!(left.free_size > 1500, "compaction failed to reclaim")
            }
            _ => panic!("expected btree leaf nodes"),
        }
        let _ = fs::remove_file(db_path);
    }
}
