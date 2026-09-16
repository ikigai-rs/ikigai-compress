//! The module recipe as one test: `ikigai-conformance` walks every endpoint
//! `ikigai_compress::space()` binds and reports every violation at once.
//!
//! Two declarations, and one honest opt-out.
//!
//! - `pure` — the compressors read nothing but their inline arguments (no file,
//!   network, clock or platform read), so a cacheable result with an empty
//!   golden-thread set is correct rather than a representation that caches
//!   forever with nothing to cut it.
//! - `cacheable` — they mark their results `.cacheable()`, which is only true
//!   because the gzip header is pinned (see the crate docs). Holding the suite to
//!   it turns a future dependency that silently downgraded the effective expiry
//!   into a red test.
//! - ⚠ **the decompressors are opted out of the checks that invoke them**, and the
//!   reason is a limitation of the harness rather than of the module:
//!   `Fixture::arg` takes a `String`, and a gzip stream is by construction not
//!   UTF-8 (byte 2 of the magic is `0x8b`). There is no way to hand a conformance
//!   fixture the one input these endpoints accept, so every check that must
//!   *resolve* the action is out of reach — for this module and for every module
//!   whose input is binary. The description-only checks (ARGSPECS, NAMES,
//!   PIPELINE, REQUIRES-VERB) still run on them; `tests/pipeline.rs` and the unit
//!   tests cover the rest end to end.

use ikigai_conformance::{Fixture, Suite};
use ikigai_core::{Kernel, Verb};
use std::sync::Arc;

/// Every endpoint `space()` binds, by description id.
const ENDPOINTS: [&str; 4] = [
    "compress-gzip",
    "decompress-gzip",
    "compress-zlib",
    "decompress-zlib",
];

/// The ids whose only input cannot be written as a `String` — see the note above.
const BINARY_INPUT: [&str; 2] = ["decompress-gzip", "decompress-zlib"];

#[test]
fn conforms() {
    let kernel = Kernel::new(Arc::new(ikigai_compress::space()));

    let mut suite = Suite::new();
    // The compressors take any bytes, so `hello` is a valid call — and once the
    // suite can resolve them it can hold them to purity and cacheability.
    for id in ["compress-gzip", "compress-zlib"] {
        suite = suite
            .pure(id)
            .cacheable(id)
            .fixture(Fixture::new(id, Verb::Source).arg("content", "hello"));
    }
    for id in BINARY_INPUT {
        suite = suite.opt_out(
            id,
            None,
            "no fixture can express this input: a gzip/zlib stream is not UTF-8 and \
             Fixture::arg takes a String. Covered by tests/pipeline.rs and the unit tests.",
        );
    }

    let report = suite.run_blocking(&kernel);
    assert!(report.is_clean(), "{report}");

    // The walk saw exactly the endpoints declared above. A fifth binding without a
    // declaration would be held to a weaker standard (the suite cannot know which
    // endpoints it was not told about); a declared id that binds nothing is a
    // stale list. Both change this count or fail the checks above.
    assert_eq!(
        report.endpoints,
        ENDPOINTS.len(),
        "every binding is declared: {report}"
    );
}
