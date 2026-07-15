Notes will generally contain things/patterns that I could use in other projects as well, or if I just 
discovered something amusing while writing this project.

## Architectural
1. `ByteCursor` to safely read and write bytes, tracking the offest sequentially.
    - holds a temporary reference `&'a buffer` to the byte slice and an offset to track where we are in the slice.
2. In `BTreeNode`, each node holds the Last Sequence Number (LSN) of the wal, why? 
    - It enforces the ARIES recovery invariant that a page's metadata must reflect the LSN of the most recent wal entry that mutated its data.
