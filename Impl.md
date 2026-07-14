## 1 Encoding Decoding && Disk Manager
1. encode-decode methods for leaf and internal nodes.
2. file open, read/write methods.

## 2 Buffer Pool & Cache Management
1. simple lru in-memory cache to avoid syscalls.
2. reading/writing in page-sized buffers.

## 3 Write-Ahead Log & Recovery
1. Provide the 'D' in acid, simplified.
2. Reloading according to the wal.

## 4 B+ Tree implementation
1. Core data manipulation methods: insert, search, delete(tombstone).
2. use rust cursor pattern thing.

## 5 Maybe I'll start with the sql part

(To be continued...)
