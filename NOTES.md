Notes will generally contain things/patterns that I could use in other projects as well, or if I just 
discovered something amusing while writing this project.

## Architectural
1. `ByteCursor` to safely read and write bytes, tracking the offest sequentially.
    - holds a temporary reference `&'a buffer` to the byte slice and an offset to track where we are 
      in the slice.
2. In `BTreeNode`, each node holds the Last Sequence Number (LSN) of the wal, why? 
    - It enforces the ARIES recovery invariant that a page's metadata must reflect the LSN of the most
      recent wal entry that mutated its data.
3. The slot array is sorted according to the `key that actually exists in [records]` for the index 
   the slot_array currently stores.
    ```rust                            
    ///                                
    /// so, if the slot_array = [0, 2, 1]  
    /// then the record order must be = [{key: 10, val: ...}, {key: 50, val: ...}, {key: 27, val: ...}]
    ```
4. If your routing key is a u64, an internal cell takes 16 bytes. A 4096-byte page holds approx 250
   child pointers. A 3-level tree can index 250^3 = 15.6 million rows with only 3 page fetches!

   If a user defines a clustered primary key on a string column like URL VARCHAR(255) or email 
   VARCHAR(150), an internal routing cell might consume 160 bytes. Your 4096-byte page now holds only 
   approx 25 child pointers. That exact same 15.6 million row table now requires a tree height of 5 or 
   6 levels! Your database instantly suffers a 100% increase in Buffer Pool cache misses and disk I/O 
   for every single point lookup.

5. One more benefit of the wal directly in our usecase, when we do multi index inserts, we'll have all
   the entries in a WalBatch which get's fsynced at once. So either the data exists completely as a 
   single write, or doesn't; there are not half-written or inconsistent data.

## To Self
1. Learn how to avoid matching on type like BTreeNode everytime you need to access the inner type, 
   either the `Borrow`, `AsRef`, or `Deref` traits help with this, or maybe even `From` and `Into`
   revise Programming Rust and google. See if you can get something like the `transpose` method even.

> Hehe, this is so fun to write.
  currently learning the routing algorithms and learning how I can introduce compaction in the pages.
