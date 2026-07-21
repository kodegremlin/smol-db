## 1 Encoding Decoding && Disk Manager
1. ~~encode-decode methods for leaf and internal nodes.~~
2. ~~file open, read/write methods.~~

## 2 Buffer Pool & Cache Management
1. ~~simple lru in-memory cache to avoid syscalls.~~
2. ~~reading/writing in page-sized buffers.~~
3. add a 75% 25% flusher that runs in the background stealing pages and writing 
them to the disk but keeping them in ram for queries but marking them clean/ready 
to be evicted on notice.

## 3 Write-Ahead Log & Recovery
1. ~~Provide the 'D' in acid, simplified.~~
2. ~~Reloading according to the wal.~~

## 4 B+ Tree implementation
1. Core data manipulation methods: insert, search, delete(tombstone).
2. use rust cursor pattern thing.

## 5 Relation Service
1. row_id aliasing to be used as PRIMARY_KEY and using that row_id for point lookups.
2. 

## 6 Secondary Indexes
1. Secondary indexes using Primary indexes only to return data, but on other fields.

(To be continued...)
