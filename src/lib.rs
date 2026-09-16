//! `ikigai-compress` — compression and decompression as resources.
//!
//! Scaffold. The module binds `urn:compress:*` and `urn:decompress:*`, in both
//! of the two shapes a compression primitive has in a resource-oriented system:
//! explicit endpoints for a caller that means to compress, and **transreptor
//! registration** so the kernel can carry a representation to and from a
//! compressed form on its own.
