use std::{
    fs::{File, OpenOptions},
    os::unix::fs::FileExt,
    path::Path,
};

use crate::error::DbError;

/// Size of an element in the index/offset array (u16).
pub const SLOT_ELEM_SIZE: usize = 2;

/// Size of any page in smol-db.
/// (we don't have true slotted page architecture)
pub const PAGE_SIZE: usize = 4096;

/// Identifier for a physical file offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PageId(pub u64);

/// An owned, 4KB buffer representing a physical slotted page.
#[derive(Debug)]
pub struct Page {
    pub data: Box<[u8; PAGE_SIZE]>,
}

impl Page {
    pub fn new() -> Self {
        Self {
            data: Box::new([0; PAGE_SIZE]),
        }
    }

    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    pub fn as_bytes_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        &mut self.data
    }
}

/// Fixed size of an internal cell (key + file_index).
pub const INDEX_ENTRY_SIZE: usize = 4 + 8;

/// Fixed size of a leaf cell metadata (key + deleted + value_size).
/// # Note
/// The actual bytes are appended after this.
pub const RECORD_META_SIZE: usize = 4 + 1 + 4;

/// Maximum allowed size of a value. Larger values than 400 will be rejected;
/// what is this? a real database (¬_¬)?
pub const MAX_VALUE_SIZE: usize = 400;

/// Fixed size of the internal node header.
pub const INTERNAL_NODE_HEADER_SIZE: usize = 1 + // cell_type (u8)
8 + // file_index (u64)
8 + // last_lsn (u64)
8 + // right_index (u64)
4 + // cell_count (u32)
2; // free_size

/// Fixed size of the leaf node header.
pub const LEAF_NODE_HEADER_SIZE: usize = 1 + // cell_type (u8)
8 + // file_index (u64)
8 + // last_lsn (u64)
1 + // has_lsib (u8 bool)
1 + // has_rsib (u8 bool)
8 + // lsib_index (u64)
8 + // rsib_index (u64)
4 + // cell_count (u32)
2; // free_size

#[repr(u8)]
pub enum NodeType {
    Internal = 0,
    Leaf = 1,
}

#[derive(Debug, Default, Clone)]
pub struct IndexEntry {
    pub key: u32,
    pub child_page_id: u64,
}

#[derive(Debug, Default, Clone)]
pub struct Record {
    pub key: u32,
    pub data: Vec<u8>,
    pub is_deleted: bool,
}

#[derive(Debug, Clone)]
pub struct InternalNode {
    pub page_id: u64,
    pub last_lsn: u64,
    pub rightmost_child: u64,
    pub slot_array: Vec<u16>,
    pub entries: Vec<IndexEntry>,
    pub free_size: u16,

    /// Tracks if the node was modified in memory. Excluded from encoding
    /// decoding.
    pub is_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct LeafNode {
    pub page_id: u64,
    pub last_lsn: u64,
    pub has_prev: bool,
    pub has_next: bool,
    pub prev_page_id: u64,
    pub next_page_id: u64,
    pub slot_array: Vec<u16>,
    pub records: Vec<Record>,
    pub free_size: u16,

    /// Tracks if the node was modified in memory. Excluded from encoding
    /// decoding.
    pub is_dirty: bool,
}

#[derive(Debug, Clone)]
pub enum BTreeNode {
    Internal(InternalNode),
    Leaf(LeafNode),
}

impl BTreeNode {
    /// Returns true if the node has been modified in memory since being loaded
    /// from disk.
    pub fn is_dirty(&self) -> bool {
        use BTreeNode::*;
        match self {
            Internal(node) => node.is_dirty,
            Leaf(node) => node.is_dirty,
        }
    }

    /// Clears the dirty flag, signifying the node to be unmodified. Must be
    /// called immediately after the Buffer Pool successfully write the page
    /// to the disk.
    pub fn clear_dirty(&mut self) {
        use BTreeNode::*;
        match self {
            Internal(node) => node.is_dirty = false,
            Leaf(node) => node.is_dirty = false,
        }
    }

    /// Marks the node as dirty :) hehe, and updates its last (LSN).
    pub fn mark_dirty(&mut self, lsn: u64) {
        use BTreeNode::*;
        match self {
            Internal(node) => {
                node.last_lsn = lsn;
                node.is_dirty = true;
            }
            Leaf(node) => {
                node.last_lsn = lsn;
                node.is_dirty = true;
            }
        }
    }

    /// Returns the LSN of the last update applied to this node.
    pub fn get_last_lsn(&self) -> u64 {
        use BTreeNode::*;
        match self {
            Internal(node) => node.last_lsn,
            Leaf(node) => node.last_lsn,
        }
    }
}

/// A minimal helper to safely read/write bytes to our 4KB array sequentially.
struct ByteCursor<'a> {
    buffer: &'a mut [u8],
    pos: usize,
}

impl<'a> ByteCursor<'a> {
    fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, pos: 0 }
    }

    fn write_u8(&mut self, val: u8) {
        self.buffer[self.pos] = val;
        self.pos += 1;
    }

    fn write_u16(&mut self, val: u16) {
        self.buffer[self.pos..self.pos + 2].copy_from_slice(&val.to_le_bytes());
        self.pos += 2;
    }

    fn write_u32(&mut self, val: u32) {
        self.buffer[self.pos..self.pos + 4].copy_from_slice(&val.to_le_bytes());
        self.pos += 4;
    }

    fn write_u64(&mut self, val: u64) {
        self.buffer[self.pos..self.pos + 8].copy_from_slice(&val.to_le_bytes());
        self.pos += 8;
    }

    fn write_bytes(&mut self, val: &[u8]) {
        self.buffer[self.pos..self.pos + val.len()].copy_from_slice(val);
        self.pos += val.len();
    }

    fn read_u8(&mut self) -> u8 {
        let val = self.buffer[self.pos];
        self.pos += 1;
        val
    }

    fn read_u16(&mut self) -> u16 {
        let val = u16::from_le_bytes(
            self.buffer[self.pos..self.pos + 2]
                .try_into()
                .unwrap(),
        );
        self.pos += 2;
        val
    }

    fn read_u32(&mut self) -> u32 {
        let val = u32::from_le_bytes(
            self.buffer[self.pos..self.pos + 4]
                .try_into()
                .unwrap(),
        );
        self.pos += 4;
        val
    }

    fn read_u64(&mut self) -> u64 {
        let val = u64::from_le_bytes(
            self.buffer[self.pos..self.pos + 8]
                .try_into()
                .unwrap(),
        );
        self.pos += 8;
        val
    }

    fn read_bytes(&mut self, len: usize) -> Vec<u8> {
        let val = self.buffer[self.pos..self.pos + len].to_vec();
        self.pos += len;
        val
    }
}

impl BTreeNode {
    pub fn decode(page: &Page) -> Result<Self, DbError> {
        let mut buffer = *page.as_bytes();
        let mut cursor = ByteCursor::new(&mut buffer);

        match cursor.read_u8() {
            // LeafNode
            1 => {
                let file_index = cursor.read_u64();
                let last_lsn = cursor.read_u64();
                let has_lsib = cursor.read_u8() != 0;
                let has_rsib = cursor.read_u8() != 0;
                let lsib_index = cursor.read_u64();
                let rsib_index = cursor.read_u64();
                let cell_count = cursor.read_u32();

                let mut indices = Vec::with_capacity(cell_count as usize);
                for _ in 0..cell_count {
                    indices.push(cursor.read_u16());
                }
                let free_size = cursor.read_u16();
                cursor.pos += free_size as usize;

                let mut cells = vec![Record::default(); cell_count as usize];

                for &cell_idx in indices.iter().take(cell_count as usize) {
                    let key = cursor.read_u32();
                    let deleted = cursor.read_u8() != 0;
                    let size = cursor.read_u32() as usize;
                    let value = cursor.read_bytes(size);
                    cells[cell_idx as usize] = Record {
                        key,
                        is_deleted: deleted,
                        data: value,
                    };
                }
                Ok(BTreeNode::Leaf(LeafNode {
                    page_id: file_index,
                    last_lsn,
                    has_prev: has_lsib,
                    has_next: has_rsib,
                    prev_page_id: lsib_index,
                    next_page_id: rsib_index,
                    slot_array: indices,
                    records: cells,
                    free_size,
                    is_dirty: false,
                }))
            }
            // InternalNode
            0 => {
                let file_index = cursor.read_u64();
                let last_lsn = cursor.read_u64();
                let right_index = cursor.read_u64();
                let cell_count = cursor.read_u32();

                let mut indices = Vec::with_capacity(cell_count as usize);
                for _ in 0..cell_count {
                    indices.push(cursor.read_u16());
                }
                let free_size = cursor.read_u16();
                cursor.pos += free_size as usize;

                let mut cells = vec![IndexEntry::default(); cell_count as usize];
                for &cell_idx in indices.iter().take(cell_count as usize) {
                    let key = cursor.read_u32();
                    let child_index = cursor.read_u64();

                    cells[cell_idx as usize] = IndexEntry {
                        key,
                        child_page_id: child_index,
                    };
                }
                Ok(BTreeNode::Internal(InternalNode {
                    page_id: file_index,
                    last_lsn,
                    rightmost_child: right_index,
                    slot_array: indices,
                    entries: cells,
                    free_size,
                    is_dirty: false,
                }))
            }
            val => Err(DbError::CorruptPage(format!("Unknown node type: {}", val))),
        }
    }

    pub fn encode(&self, page: &mut Page) -> Result<(), DbError> {
        let mut cursor = ByteCursor::new(page.as_bytes_mut());
        match self {
            BTreeNode::Leaf(node) => {
                let header_len = LEAF_NODE_HEADER_SIZE + (node.slot_array.len() * SLOT_ELEM_SIZE);
                let footer_len: usize = node
                    .slot_array
                    .iter()
                    .map(|&cell_idx| {
                        let cell = &node.records[cell_idx as usize];
                        RECORD_META_SIZE + cell.data.len()
                    })
                    .sum();
                if header_len + footer_len > PAGE_SIZE {
                    return Err(DbError::PageFull);
                }
                let free_size = (PAGE_SIZE - header_len - footer_len) as u16;

                cursor.write_u8(NodeType::Leaf as u8);
                cursor.write_u64(node.page_id);
                cursor.write_u64(node.last_lsn);
                cursor.write_u8(node.has_prev as u8);
                cursor.write_u8(node.has_next as u8);
                cursor.write_u64(node.prev_page_id);
                cursor.write_u64(node.next_page_id);

                cursor.write_u32(node.slot_array.len() as u32);
                for &index in &node.slot_array {
                    cursor.write_u16(index);
                }
                cursor.write_u16(free_size);
                cursor.pos += free_size as usize;

                for &index in &node.slot_array {
                    let cell = &node.records[index as usize];
                    cursor.write_u32(cell.key);
                    cursor.write_u8(cell.is_deleted as u8);
                    cursor.write_u32(cell.data.len() as u32);
                    cursor.write_bytes(&cell.data);
                }
            }
            BTreeNode::Internal(node) => {
                let header_len =
                    INTERNAL_NODE_HEADER_SIZE + (node.slot_array.len() * SLOT_ELEM_SIZE);
                let footer_len = node.slot_array.len() * INDEX_ENTRY_SIZE;

                if header_len + footer_len > PAGE_SIZE {
                    return Err(DbError::PageFull);
                }
                let free_size = (PAGE_SIZE - header_len - footer_len) as u16;

                cursor.write_u8(NodeType::Internal as u8);
                cursor.write_u64(node.page_id);
                cursor.write_u64(node.last_lsn);
                cursor.write_u64(node.rightmost_child);

                cursor.write_u32(node.slot_array.len() as u32);
                for &offset in &node.slot_array {
                    cursor.write_u16(offset);
                }
                cursor.write_u16(free_size);
                cursor.pos += free_size as usize;

                for &offset in &node.slot_array {
                    let cell = &node.entries[offset as usize];
                    cursor.write_u32(cell.key);
                    cursor.write_u64(cell.child_page_id);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct FileHeader {
    pub last_row_id: u32,
    pub page_root_offset: u64,
    pub next_lsn: u64,
    pub next_free_offset: u64,
}

impl Default for FileHeader {
    fn default() -> Self {
        Self {
            last_row_id: 0,
            page_root_offset: 0,
            next_lsn: 0,
            next_free_offset: PAGE_SIZE as u64,
        }
    }
}

#[derive(Debug)]
pub struct DiskManager {
    file: File,
    pub header: FileHeader,
}

impl DiskManager {
    pub fn new<P>(path: P) -> Result<Self, DbError>
    where
        P: AsRef<Path>,
    {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;

        let mut header = FileHeader::default();
        let metadata = file.metadata()?;

        if metadata.len() > 0 {
            let mut buffer = [0u8; 28]; // 4 + 8 + 8 + 8 = 28
            file.read_exact_at(&mut buffer, 0)?;

            header.last_row_id = u32::from_le_bytes(buffer[0..4].try_into().unwrap());
            header.page_root_offset = u64::from_le_bytes(buffer[4..12].try_into().unwrap());
            header.next_lsn = u64::from_le_bytes(buffer[12..20].try_into().unwrap());
            header.next_free_offset = u64::from_le_bytes(buffer[20..28].try_into().unwrap());
        }
        Ok(Self { file, header })
    }

    pub fn read_page(&self, page_id: &PageId) -> Result<Page, DbError> {
        let mut page = Page::new();
        self.file
            .read_exact_at(page.as_bytes_mut(), page_id.0)?;
        Ok(page)
    }

    pub fn write_page(&self, page_id: PageId, page: &Page) -> Result<(), DbError> {
        self.file
            .write_all_at(page.as_bytes(), page_id.0)?;
        Ok(())
    }

    pub fn allocate_page(&mut self) -> PageId {
        let new_page_id = PageId(self.header.next_free_offset);
        self.header.next_free_offset += PAGE_SIZE as u64;
        new_page_id
    }

    pub fn save_header(&self) -> Result<(), DbError> {
        let mut buffer = [0u8; 28];

        buffer[0..4].copy_from_slice(&self.header.last_row_id.to_le_bytes());
        buffer[4..12].copy_from_slice(&self.header.page_root_offset.to_le_bytes());
        buffer[12..20].copy_from_slice(&self.header.next_lsn.to_le_bytes());
        buffer[20..28].copy_from_slice(&self.header.next_free_offset.to_le_bytes());

        self.file.write_all_at(&buffer, 0)?;
        self.file.sync_all()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fs::remove_file;
    use std::time::SystemTime;

    /// Helper to generate a unique temporary file path for concurrent DiskManager tests.
    fn temp_db_path(test_name: &str) -> String {
        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("/tmp/smol_db_test_{}_{}.db", test_name, timestamp)
    }

    /// Helper to construct a dummy Record with arbitrary key and data payload.
    fn make_record(key: u32, data: &[u8], is_deleted: bool) -> Record {
        Record {
            key,
            data: data.to_vec(),
            is_deleted,
        }
    }

    /// Verifies that ByteCursor correctly reads and writes mixed endian integers and slices sequentially.
    #[test]
    fn test_byte_cursor_primitives() {
        let mut buffer = [0u8; 64];
        let mut write_cursor = ByteCursor::new(&mut buffer);

        write_cursor.write_u8(42);
        write_cursor.write_u16(u16::MAX);
        write_cursor.write_u32(123456789);
        write_cursor.write_u64(u64::MAX);
        write_cursor.write_bytes(b"smol-db");

        let mut read_cursor = ByteCursor::new(&mut buffer);
        assert_eq!(read_cursor.read_u8(), 42);
        assert_eq!(read_cursor.read_u16(), u16::MAX);
        assert_eq!(read_cursor.read_u32(), 123456789);
        assert_eq!(read_cursor.read_u64(), u64::MAX);
        assert_eq!(read_cursor.read_bytes(7), b"smol-db");
    }

    /// Validates round-trip encoding and decoding of an empty LeafNode without siblings.
    #[test]
    fn test_empty_leaf_node_round_trip() -> Result<(), Box<dyn Error>> {
        let leaf = LeafNode {
            page_id: 1,
            last_lsn: 100,
            has_prev: false,
            has_next: false,
            prev_page_id: 0,
            next_page_id: 0,
            slot_array: vec![],
            records: vec![],
            free_size: 0, // Will be recalculated during encode
            is_dirty: false,
        };

        let mut page = Page::new();
        BTreeNode::Leaf(leaf.clone()).encode(&mut page)?;

        let decoded = BTreeNode::decode(&page)?;
        if let BTreeNode::Leaf(dec_leaf) = decoded {
            assert_eq!(dec_leaf.page_id, leaf.page_id);
            assert_eq!(dec_leaf.last_lsn, leaf.last_lsn);
            assert!(!dec_leaf.has_prev);
            assert!(!dec_leaf.has_next);
            assert!(dec_leaf.slot_array.is_empty());
            assert!(dec_leaf.records.is_empty());
            // Header is 33 bytes; free size should be PAGE_SIZE - 33
            assert_eq!(
                dec_leaf.free_size as usize,
                PAGE_SIZE - LEAF_NODE_HEADER_SIZE
            );
        } else {
            panic!("Expected BTreeNode::Leaf, got Internal");
        }

        Ok(())
    }

    /// Verifies round-trip encoding of a LeafNode containing multiple records, including deleted flags and sibling pointers.
    #[test]
    fn test_populated_leaf_node_round_trip() -> Result<(), Box<dyn Error>> {
        let records = vec![
            make_record(10, b"alice", false),
            make_record(20, b"bob-was-deleted", true),
            make_record(30, b"charlie", false),
        ];

        let leaf = LeafNode {
            page_id: 42,
            last_lsn: 999,
            has_prev: true,
            has_next: true,
            prev_page_id: 41,
            next_page_id: 43,
            // Slot array maps logical sorted order to physical record vector indices
            slot_array: vec![0, 1, 2],
            records: records.clone(),
            free_size: 0,
            is_dirty: false,
        };

        let mut page = Page::new();
        BTreeNode::Leaf(leaf).encode(&mut page)?;

        let decoded = BTreeNode::decode(&page)?;
        if let BTreeNode::Leaf(dec_leaf) = decoded {
            assert_eq!(dec_leaf.page_id, 42);
            assert_eq!(dec_leaf.prev_page_id, 41);
            assert_eq!(dec_leaf.next_page_id, 43);
            assert_eq!(dec_leaf.slot_array, vec![0, 1, 2]);
            assert_eq!(dec_leaf.records.len(), 3);

            assert_eq!(dec_leaf.records[0].key, 10);
            assert_eq!(dec_leaf.records[0].data, b"alice");
            assert!(!dec_leaf.records[0].is_deleted);

            assert_eq!(dec_leaf.records[1].key, 20);
            assert_eq!(dec_leaf.records[1].data, b"bob-was-deleted");
            assert!(dec_leaf.records[1].is_deleted);
        } else {
            panic!("Expected BTreeNode::Leaf");
        }

        Ok(())
    }

    /// Ensures attempting to encode data exceeding the 4KB page boundary returns DbError::PageFull.
    #[test]
    fn test_leaf_node_overflow_returns_page_full() {
        // Create 11 records of 400 bytes each (11 * 400 = 4400 bytes > 4096 PAGE_SIZE)
        let mut records = Vec::new();
        let mut slot_array = Vec::new();
        for i in 0..11 {
            records.push(make_record(i, &[0u8; MAX_VALUE_SIZE], false));
            slot_array.push(i as u16);
        }

        let leaf = LeafNode {
            page_id: 1,
            last_lsn: 0,
            has_prev: false,
            has_next: false,
            prev_page_id: 0,
            next_page_id: 0,
            slot_array,
            records,
            free_size: 0,
            is_dirty: false,
        };

        let mut page = Page::new();
        let result = BTreeNode::Leaf(leaf).encode(&mut page);

        assert!(matches!(result, Err(DbError::PageFull)));
    }

    /// Validates round-trip encoding and decoding of an InternalNode with routing keys and child pointers.
    #[test]
    fn test_internal_node_round_trip() -> Result<(), Box<dyn Error>> {
        let entries = vec![
            IndexEntry {
                key: 100,
                child_page_id: 2,
            },
            IndexEntry {
                key: 200,
                child_page_id: 3,
            },
            IndexEntry {
                key: 300,
                child_page_id: 4,
            },
        ];

        let internal = InternalNode {
            page_id: 1,
            last_lsn: 555,
            rightmost_child: 5,
            slot_array: vec![0, 1, 2],
            entries: entries.clone(),
            free_size: 0,
            is_dirty: false,
        };

        let mut page = Page::new();
        BTreeNode::Internal(internal).encode(&mut page)?;

        let decoded = BTreeNode::decode(&page)?;
        if let BTreeNode::Internal(dec_internal) = decoded {
            assert_eq!(dec_internal.page_id, 1);
            assert_eq!(dec_internal.last_lsn, 555);
            assert_eq!(dec_internal.rightmost_child, 5);
            assert_eq!(dec_internal.slot_array.len(), 3);
            assert_eq!(dec_internal.entries[1].key, 200);
            assert_eq!(dec_internal.entries[1].child_page_id, 3);
        } else {
            panic!("Expected BTreeNode::Internal");
        }

        Ok(())
    }

    /// Ensures decoding a page with an invalid magic byte returns a CorruptPage error variant.
    #[test]
    fn test_decode_corrupt_node_type_fails() {
        let mut page = Page::new();
        page.as_bytes_mut()[0] = 99; // Invalid node type magic byte (valid is 0 or 1)

        let result = BTreeNode::decode(&page);
        assert!(matches!(result, Err(DbError::CorruptPage(_))));
    }

    /// Verifies end-to-end persistence: allocating, writing, and reading pages via DiskManager.
    #[test]
    fn test_disk_manager_page_lifecycle() -> Result<(), Box<dyn Error>> {
        let path = temp_db_path("disk_manager_lifecycle");
        let mut dm = DiskManager::new(&path)?;

        // Allocate two physical pages
        let page_id_1 = dm.allocate_page();
        let page_id_2 = dm.allocate_page();
        assert_ne!(page_id_1, page_id_2);

        // Encode a leaf node onto page 1
        let mut page_1 = Page::new();
        let leaf = LeafNode {
            page_id: page_id_1.0,
            last_lsn: 10,
            has_prev: false,
            has_next: false,
            prev_page_id: 0,
            next_page_id: 0,
            slot_array: vec![0],
            records: vec![make_record(1, b"disk-test", false)],
            free_size: 0,
            is_dirty: false,
        };
        BTreeNode::Leaf(leaf).encode(&mut page_1)?;
        dm.write_page(page_id_1, &page_1)?;

        // Read page 1 back from disk and decode
        let read_page = dm.read_page(&page_id_1)?;
        let decoded = BTreeNode::decode(&read_page)?;
        if let BTreeNode::Leaf(dec_leaf) = decoded {
            assert_eq!(dec_leaf.records[0].data, b"disk-test");
        } else {
            panic!("Failed to decode written leaf page from DiskManager");
        }

        // Clean up temporary artifact
        remove_file(path)?;
        Ok(())
    }

    /// Validates that DiskManager correctly persists and restores the 28-byte FileHeader across file reopens.
    #[test]
    fn test_disk_manager_header_persistence() -> Result<(), Box<dyn Error>> {
        let path = temp_db_path("header_persistence");

        {
            let mut dm = DiskManager::new(&path)?;
            dm.header.last_row_id = 42;
            dm.header.page_root_offset = 8192;
            dm.header.next_lsn = 1000;
            dm.header.next_free_offset = 16384;
            dm.save_header()?;
        } // Drop file handle

        // Reopen disk manager on the existing file
        let dm_reopened = DiskManager::new(&path)?;
        assert_eq!(dm_reopened.header.last_row_id, 42);
        assert_eq!(dm_reopened.header.page_root_offset, 8192);
        assert_eq!(dm_reopened.header.next_lsn, 1000);
        assert_eq!(dm_reopened.header.next_free_offset, 16384);

        remove_file(path)?;
        Ok(())
    }
}
