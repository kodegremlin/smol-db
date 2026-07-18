use std::{
    fs::{File, OpenOptions},
    io::{BufReader, ErrorKind, Read, Seek, SeekFrom, Write},
    path::Path,
};

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

    /// Returns the size of the entire entry including the headers.
    pub fn size(&self) -> usize {
        WAL_ENTRY_HEADER_SIZE + self.payload.len()
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

/// Manages the append-only Write-Ahead Log (Wal) file on disk.
///
/// Uses a length-prefixed stream framing ([length:u32][payload])
/// for clean boundaries and facilitate crash recovery.
pub struct WalManager {
    file: File,
    sync: bool,
}

impl WalManager {
    /// Opens an existing wal file in append mode or creates a new one returning
    /// `WalManager` with the fields initialized.
    pub fn open<P: AsRef<Path>>(path: P, sync: bool) -> Result<Self, DbError> {
        Ok(Self {
            file: OpenOptions::new()
                .create(true)
                .append(true)
                .read(true)
                .open(path)?,
            sync,
        })
    }

    /// Explicitly syncs the file regardless of the `sync` flag being true or
    /// false.
    pub fn sync(&mut self) -> Result<(), DbError> {
        self.file.sync_all()?;
        Ok(())
    }

    /// Appends a batch of log entries to the Wal file using 4-byte little-endian
    /// length indicator for each entry.
    pub fn write_batch(&mut self, batch: &WalBatch) -> Result<(), DbError> {
        if batch.is_empty() {
            return Ok(());
        }
        // Calculate the exact memory capacity required for this entire batch.
        let net_size = batch.iter().map(|entry| 4 + entry.size()).sum();

        let mut buffer = Vec::with_capacity(net_size);
        for entry in batch {
            let encoded_entry = entry.encode();
            let entry_len = encoded_entry.len() as u32;

            // prepend the size of this entry.
            buffer.extend_from_slice(&entry_len.to_le_bytes());
            buffer.extend_from_slice(&encoded_entry);
        }
        debug_assert_eq!(
            buffer.len(),
            net_size,
            "final buffer len does not matches expected size"
        );
        self.file.write_all(&buffer)?;

        if self.sync {
            self.file.sync_all()?;
        }
        Ok(())
    }

    /// Reads the entire Wal file, parsing length-prefixed entries into a batch.
    /// Terminates cleanly when encountering EOF or 0 for length-prefix.
    pub fn read_batch(&mut self) -> Result<WalBatch, DbError> {
        let mut batch = WalBatch::new();

        // Ensure we start reading from the very beginning of the log file.
        self.file.seek(SeekFrom::Start(0))?;

        let mut reader = BufReader::new(&self.file);
        let mut len_buf = [0u8; 4];

        use ErrorKind::*;
        loop {
            // Attempt to read length indicator of exactly 4 bytes.
            match reader.read_exact(&mut len_buf) {
                Err(err) if err.kind() == UnexpectedEof => {
                    // Expected: we reached the end of log file.
                    break;
                }
                Err(err) => return Err(DbError::Io(err)),
                Ok(_) => {}
            }
            let entry_len = u32::from_le_bytes(len_buf) as usize;

            // zero length prefix means end of valid log data.
            if entry_len == 0 {
                break;
            }
            let mut buffer = vec![0u8; entry_len];

            // Read the exact payload bytes into buffer.
            match reader.read_exact(&mut buffer) {
                Err(err) if err.kind() == UnexpectedEof => {
                    return Err(DbError::CorruptPage(format!(
                        "invalid wal detected: header reported {} bytes, buf file ended unexpectedly",
                        entry_len
                    )));
                }
                Err(err) => return Err(DbError::Io(err)),
                Ok(_) => {}
            }
            let entry = WalEntry::decode(&buffer)?;
            batch.push(entry);
        }
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::*;

    fn get_temp_wal_path(name: &str) -> PathBuf {
        let mut path = PathBuf::from("/Volumes/External T7/");
        path.push(format!("smol-db_test_wal_{}.log", name));
        let _ = fs::remove_file(&path);
        path
    }

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

    #[test]
    fn test_wal_manager_batch_persistence_and_framing() {
        let path = get_temp_wal_path("persistence");

        dbg!(&path);

        let batch_out = vec![
            WalEntry {
                opcode: WalOp::Insert,
                lsn: 1,
                page_id: PageId(4096),
                row_id: 100,
                payload: vec![1, 2, 3],
            },
            WalEntry {
                opcode: WalOp::Update,
                lsn: 2,
                page_id: PageId(8192),
                row_id: 101,
                payload: vec![4, 5, 6],
            },
            WalEntry {
                opcode: WalOp::Delete,
                lsn: 3,
                page_id: PageId(4096),
                row_id: 100,
                payload: vec![],
            },
        ];
        {
            let mut wal = WalManager::open(&path, true).expect("failed to open Wal");
            wal.write_batch(&batch_out)
                .expect("failed to flush Wal batch");
            wal.sync().expect("failed to close Wal");
        }
        {
            let mut wal = WalManager::open(&path, true).expect("failed to reopen Wal");
            let batch_in = wal
                .read_batch()
                .expect("failed to read Wal entries");
            assert_eq!(batch_in.len(), 3, "should read exactly 3 framed entries");
            assert_eq!(
                batch_out, batch_in,
                "decoded batch must match written batch exactly"
            );
        }
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn test_wal_manager_torn_write_detection() {
        let path = get_temp_wal_path("torn_write");

        // construct a corrupt file intentionally.
        {
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .open(&path)
                .unwrap();

            // lie about the header length.
            let wrong_len = 50u32;
            file.write_all(&wrong_len.to_le_bytes()).unwrap();

            // attempt to write a byte less.
            file.write_all(&[0u8; 49]).unwrap();
            file.sync_all().unwrap();
        }
        {
            let mut wal = WalManager::open(&path, false).expect("failed to open wal");
            let result = wal.read_batch();
            assert!(
                result.is_err(),
                "reading a torn write should've triggered a corruption error"
            );
            if let Err(DbError::CorruptPage(msg)) = result {
                assert!(msg.contains("invalid wal detected"));
            } else {
                panic!("expected DbError::CorruptPage, got a different error");
            }
        }
        let _ = fs::remove_file(&path);
    }
}
