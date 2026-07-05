use crate::error::DbError;

/// Size of an element in the index/offset array (u16).
pub const OFFSET_ELEM_SIZE: usize = 2;

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
pub const INTERNAL_CELL_SIZE: usize = 4 + 8;

/// Fixed size of a leaf cell metadata (key + deleted + value_size).
/// # Note
/// The actual bytes are appended after this.
pub const LEAF_CELL_META_SIZE: usize = 4 + 1 + 4;

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
pub struct InternalCell {
    pub key: u32,
    pub child_index: u64,
}

#[derive(Debug, Default, Clone)]
pub struct LeafCell {
    pub key: u32,
    pub deleted: bool,
    pub value: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct InternalNode {
    pub file_index: u64,
    pub last_lsn: u64,
    pub right_index: u64,
    pub indices: Vec<u16>,
    pub cells: Vec<InternalCell>,
    pub free_size: u16,
}

#[derive(Debug, Clone)]
pub struct LeafNode {
    pub file_index: u64,
    pub last_lsn: u64,
    pub has_lsib: bool,
    pub has_rsib: bool,
    pub lsib_index: u64,
    pub rsib_index: u64,
    pub indices: Vec<u16>,
    pub cells: Vec<LeafCell>,
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

                let mut cells = vec![LeafCell::default(); cell_count as usize];

                for &cell_idx in indices.iter().take(cell_count as usize) {
                    let key = cursor.read_u32();
                    let deleted = cursor.read_u8() != 0;
                    let size = cursor.read_u32() as usize;
                    let value = cursor.read_bytes(size);
                    cells[cell_idx as usize] = LeafCell { key, deleted, value };
                }
                Ok(BTreeNode::Leaf(LeafNode {
                    file_index,
                    last_lsn,
                    has_lsib,
                    has_rsib,
                    lsib_index,
                    rsib_index,
                    indices,
                    cells,
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

                let mut cells = vec![InternalCell::default(); cell_count as usize];
                for &cell_idx in indices.iter().take(cell_count as usize) {
                    let key = cursor.read_u32();
                    let child_index = cursor.read_u64();

                    cells[cell_idx as usize] = InternalCell { key, child_index };
                }
                Ok(BTreeNode::Internal(InternalNode {
                    file_index,
                    last_lsn,
                    right_index,
                    indices,
                    cells,
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
                let header_len = LEAF_NODE_HEADER_SIZE + (node.indices.len() * OFFSET_ELEM_SIZE);
                let footer_len: usize = node
                    .indices
                    .iter()
                    .map(|&cell_idx| {
                        let cell = &node.cells[cell_idx as usize];
                        LEAF_CELL_META_SIZE + cell.value.len()
                    })
                    .sum();
                if header_len + footer_len > PAGE_SIZE {
                    return Err(DbError::PageFull);
                }
                let free_size = (PAGE_SIZE - header_len - footer_len) as u16;

                cursor.write_u8(NodeType::Leaf as u8);
                cursor.write_u64(node.file_index);
                cursor.write_u64(node.last_lsn);
                cursor.write_u8(node.has_lsib as u8);
                cursor.write_u8(node.has_rsib as u8);
                cursor.write_u64(node.lsib_index);
                cursor.write_u64(node.rsib_index);

                cursor.write_u32(node.indices.len() as u32);
                for &index in &node.indices {
                    cursor.write_u16(index);
                }
                cursor.write_u16(free_size);
                cursor.pos += free_size as usize;

                for &index in &node.indices {
                    let cell = &node.cells[index as usize];
                    cursor.write_u32(cell.key);
                    cursor.write_u8(cell.deleted as u8);
                    cursor.write_u32(cell.value.len() as u32);
                    cursor.write_bytes(&cell.value);
                }
            }
            BTreeNode::Internal(node) => {
                let header_len =
                    INTERNAL_NODE_HEADER_SIZE + (node.indices.len() * OFFSET_ELEM_SIZE);
                let footer_len = node.indices.len() * INTERNAL_CELL_SIZE;

                if header_len + footer_len > PAGE_SIZE {
                    return Err(DbError::PageFull);
                }
                let free_size = (PAGE_SIZE - header_len - footer_len) as u16;

                cursor.write_u8(NodeType::Internal as u8);
                cursor.write_u64(node.file_index);
                cursor.write_u64(node.last_lsn);
                cursor.write_u64(node.right_index);

                cursor.write_u32(node.indices.len() as u32);
                for &offset in &node.indices {
                    cursor.write_u16(offset);
                }
                cursor.write_u16(free_size);
                cursor.pos += free_size as usize;

                for &offset in &node.indices {
                    let cell = &node.cells[offset as usize];
                    cursor.write_u32(cell.key);
                    cursor.write_u64(cell.child_index);
                }
            }
        }
        Ok(())
    }
}
