Notes will generally contain things/patterns that I could use in other projects as well, or if I just 
discovered something amusing while writing this project.

## Architectural
1. `ByteCursor` to safely read and write bytes, tracking the offest sequentially.
    - holds a temporary reference `&'a buffer` to the byte slice and an offset to track where we are in the slice.
2. In `BTreeNode`, each node holds the Last Sequence Number (LSN) of the wal, why? 
    - It enforces the ARIES recovery invariant that a page's metadata must reflect the LSN of the most recent wal entry that mutated its data.
3. The slot array is sorted according to the `key that actually exists in [records]` for the index the slot_array currently stores.
    ```rust                            
    ///                                
    /// so, if the slot_array = [0, 2, 1]  
    /// then the record order must be = [{key: 10, val: ...}, {key: 50, val: ...}, {key: 27, val: ...}]
    ```
