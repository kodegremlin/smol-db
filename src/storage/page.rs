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
}

#[derive(Debug, Clone)]
pub enum BTreeNode {
    Internal(InternalNode),
    Leaf(LeafNode),
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
