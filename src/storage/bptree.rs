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

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
    };

    use crate::storage::{
        bptree::BpTree,
        buffer_pool::BufferPool,
        page::{BTreeNode, DiskManager, IndexEntry, PageId, Record},
    };

    fn setup_pool(dir_path: &Path) -> BufferPool {
        let db_path = dir_path.join("test.tbl");
        let _ = fs::remove_file(&db_path);
        let disk_manager = DiskManager::open(&db_path).unwrap();
        BufferPool::new(disk_manager, 10, None)
    }

    /// Manually construct a 2-level B+ Tree on disk:
    /// Root (page 0, internal): key 20 -> left child (page 1), right child (page 2)
    /// Left child (page 1, leaf): keys [10, 15]
    /// Right child (page 2, leaf): keys [20, 25 (deleted), 30]
    fn build_test_tree(pool: &mut BufferPool) -> PageId {
        // 1. Allocate Left Leaf Page legitimately through the Buffer Pool
        let (left_id, left_frame) = pool.new_page(true).unwrap();
        {
            let mut left_leaf_guard = left_frame.write();
            if let BTreeNode::Leaf(ref mut leaf) = *left_leaf_guard {
                leaf.records.push(Record {
                    key: 10,
                    is_deleted: false,
                    data: vec![1, 0],
                });
                leaf.records.push(Record {
                    key: 15,
                    is_deleted: false,
                    data: vec![1, 5],
                });
                leaf.slot_array = vec![0, 1];
            }
            left_leaf_guard.mark_dirty(0);
        }

        let (right_id, right_frame) = pool.new_page(true).unwrap();
        {
            let mut right_leaf_guard = right_frame.write();
            if let BTreeNode::Leaf(ref mut leaf) = *right_leaf_guard {
                leaf.records.push(Record {
                    key: 20,
                    is_deleted: false,
                    data: vec![2, 0],
                });
                leaf.records.push(Record {
                    key: 30,
                    is_deleted: false,
                    data: vec![3, 0],
                });
                leaf.records.push(Record {
                    key: 25,
                    is_deleted: true,
                    data: vec![2, 5],
                }); // Tombstone
                leaf.slot_array = vec![0, 2, 1];

                // Wire backward sibling link
                leaf.has_prev = true;
                leaf.prev_page_id = left_id.into();
            }
            right_leaf_guard.mark_dirty(0);
        }
        {
            let mut left_leaf_guard = left_frame.write();
            if let BTreeNode::Leaf(ref mut leaf) = *left_leaf_guard {
                leaf.has_next = true;
                leaf.next_page_id = right_id.into();
            }
            left_leaf_guard.mark_dirty(0);
        }

        let (root_id, root_frame) = pool.new_page(false).unwrap();
        {
            let mut root_guard = root_frame.write();
            if let BTreeNode::Internal(ref mut node) = *root_guard {
                node.rightmost_child_id = right_id;
                node.entries.push(IndexEntry {
                    key: 20,
                    child_page_id: left_id.into(),
                });
                node.slot_array = vec![0];
            }
            root_guard.mark_dirty(0);
        }
        pool.flush_all_pages().unwrap();
        root_id
    }

    #[test]
    fn test_btree_find_cell_routing_and_tombstones() {
        let dir = PathBuf::from("/Volumes/External T7/");
        let mut pool = setup_pool(&dir);
        let root_id = build_test_tree(&mut pool);
        dbg!(&root_id);
        let mut btree = BpTree::new(&mut pool, root_id);

        let val = btree
            .find_record(10)
            .expect("RESULT: lookup failed");

        assert_eq!(val, Some(vec![1, 0]));

        let val = btree
            .find_record(30)
            .expect("RESULT: lookup failed");

        assert_eq!(val, Some(vec![3, 0]));

        let val = btree
            .find_record(25)
            .expect("RESULT: lookup failed");

        assert_eq!(val, None, "deleted cell should return None");

        let val = btree
            .find_record(99)
            .expect("RESULT: lookup failed");

        assert_eq!(val, None);

        let val = btree
            .find_record(15)
            .expect("RESULT: lookup failed");

        assert_eq!(val, Some(vec![1, 5]));
    }
}
