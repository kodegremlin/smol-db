use crate::{error::DbError, storage::page::PageId};

/// Fixed size of the physical Wal entry header in bytes.
/// op(1) + lsn(8) + page_id(8) + row_id(4) + val_len(4) = 25 bytes.
pub const WAL_ENTRY_HEADER_SIZE: usize = 25;

/// Represents the physical operation recorded in the log.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalOp {
    Insert = 0,
    Update = 1,
    Delete = 2,
}

impl WalOp {
    /// Converts a single raw byte into a WalOp enum type.
    pub fn from_u8(val: u8) -> Result<Self, DbError> {
        match val {
            0 => Ok(Self::Insert),
            1 => Ok(Self::Update),
            2 => Ok(Self::Delete),
            _ => Err(DbError::CorruptPage(format!(
                "invalid WalOp byte discriminator: {}",
                val
            ))),
        }
    }

    /// Exposes the underlying byte representation for serialization.
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

/// A physical log record describing an atomic row modification on a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalEntry {
    pub opcode: WalOp,
    pub lsn: u64,
    pub page_id: PageId,
    pub row_id: u32,
    pub payload: Vec<u8>,
}

impl WalEntry {
    /// Serializes the log entry into a raw little-endian byte vector.
    /// [WAL_ENTRY_HEADER_SIZE] + \[payload: Vec<u8>\]
    pub fn encode(&self) -> Vec<u8> {
        let net_size = WAL_ENTRY_HEADER_SIZE + self.payload.len();
        let mut buffer = Vec::with_capacity(net_size);

        buffer.push(self.opcode.as_u8());

        buffer.extend_from_slice(&self.lsn.to_le_bytes());
        buffer.extend_from_slice(&self.page_id.0.to_le_bytes());
        buffer.extend_from_slice(&self.row_id.to_le_bytes());

        let payload_len = self.payload.len() as u32;
        buffer.extend_from_slice(&payload_len.to_le_bytes());

        buffer.extend_from_slice(&self.payload);

        debug_assert_eq!(buffer.len(), net_size, "encoded Wal entry length mismatch");
        buffer
    }

    /// Deserialize a raw little-endian byte slice into a `WalEntry`.
    pub fn decode(buffer: &[u8]) -> Result<Self, DbError> {
        if buffer.len() < WAL_ENTRY_HEADER_SIZE {
            return Err(DbError::CorruptPage(format!(
                "Wal entry buffer too small: expected at least {} bytes, got {}",
                WAL_ENTRY_HEADER_SIZE,
                buffer.len()
            )));
        }
        let opcode = WalOp::from_u8(buffer[0])?;
        let lsn = u64::from_le_bytes(buffer[1..9].try_into().unwrap());
        let page_id = PageId(u64::from_le_bytes(buffer[9..17].try_into().unwrap()));
        let row_id = u32::from_le_bytes(buffer[17..21].try_into().unwrap());
        let payload_len = u32::from_le_bytes(buffer[21..25].try_into().unwrap()) as usize;

        // Verify net buffer size against calculated payload size.
        let expected_size = WAL_ENTRY_HEADER_SIZE + payload_len;
        if buffer.len() < expected_size {
            return Err(DbError::CorruptPage(format!(
                "Wal entry payload smaller than calculated size: expected {} bytes, got {}",
                expected_size,
                buffer.len()
            )));
        }
        let payload = buffer[25..expected_size].to_vec();
        Ok(Self {
            opcode,
            lsn,
            page_id,
            row_id,
            payload,
        })
    }
}

/// A collection of log entries to be represented as an atomic batch.
pub type WalBatch = Vec<WalEntry>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wal_entry_symmetry_and_layout() {
        let original_entry = WalEntry {
            opcode: WalOp::Update,
            lsn: 1048576,
            page_id: PageId(8192),
            row_id: 42,
            payload: vec![10, 20, 30, 40],
        };

        let encoded = original_entry.encode();

        // Verify exact byte sizing: 25 header bytes + 4 payload bytes = 29 bytes
        assert_eq!(
            encoded.len(),
            29,
            "encoded buffer size must match header + payload length"
        );

        // Verify operation discriminator at byte 0
        assert_eq!(encoded[0], WalOp::Update as u8);

        let decoded = WalEntry::decode(&encoded).expect("failed to decode valid WAL entry");
        assert_eq!(
            original_entry, decoded,
            "decoded entry must match original entry exactly"
        );
    }

    #[test]
    fn test_wal_entry_decode_truncated_buffer() {
        let short_buf = vec![0u8; 10]; // Smaller than 25-byte header
        let result = WalEntry::decode(&short_buf);
        assert!(
            result.is_err(),
            "decoding a truncated header buffer must fail"
        );
    }
}
