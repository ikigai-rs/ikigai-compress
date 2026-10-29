//! The module recipe as one test: `ikigai-conformance` walks every endpoint
//! `ikigai_compress::space()` binds and reports every violation at once.
//!
//! Two declarations, a fixture for every endpoint, three waived checks, the body
//! wiring, and the space's name.
//!
//! - `pure` — the compressors read nothing but their inline arguments (no file,
//!   network, clock or platform read), so a cacheable result with an empty
//!   golden-thread set is correct rather than a representation that caches
//!   forever with nothing to cut it.
//! - `cacheable` — they mark their results `.cacheable()`, which is only true
//!   because the gzip header is pinned (see the crate docs). Holding the suite to
//!   it turns a future dependency that silently downgraded the effective expiry
//!   into a red test.
//! - the decompressors get a **binary** fixture (`Fixture::arg_bytes`, since
//!   conformance 0.8.1): the gzip/zlib stream of `hello`, compressed by this
//!   crate's own compressor through the kernel. Until 0.12.0 this test opted them
//!   out of every invoking check because a fixture value had to be a `String` and
//!   a gzip stream is not UTF-8; that has not been true since 0.8.1, and with the
//!   opt-out in place `BODIES` could not probe them either. They are pure and
//!   cacheable for the same reason the compressors are. Three checks are waived
//!   on them, with a reason: `SKOLEM-RDF`, `VOCABULARY` and `DATATYPES`, which
//!   all parse each RDF face, and their RDF outputs are only the caller's `as=` label on a
//!   payload this module cannot make RDF.
//! - `BODIES` gets the module space through `Suite::body_space`, the SAME `Arc`
//!   the kernel was built from, so it compares each endpoint's `invoke` with its
//!   `invoke_body` and holds a one-byte-short budget to `OverBudget`.
//!   `BODY-INPUTS` has nothing to probe: no argument is declared `reads_body`
//!   (`content` is read inline, `Invocation::inline_arg`), so no content store
//!   is wired. `Expected::nothing_unprobed` keeps the wiring from falling off.
//! - `space()` is **self-named** `urn:iki:space:compress`: it takes no
//!   parameters and reads nothing while it is built, so every call holds the
//!   same doors and the name is a true claim.

use futures::executor::block_on;
use ikigai_conformance::{Check, Expected, Fixture, Suite};
use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
use std::sync::Arc;

/// Every endpoint `space()` binds, by description id.
const ENDPOINTS: [&str; 4] = [
    "compress-gzip",
    "decompress-gzip",
    "compress-zlib",
    "decompress-zlib",
];

/// Each decompressor with the compressor whose output it reads.
const PAIRS: [(&str, &str); 2] = [
    ("decompress-gzip", "urn:compress:gzip"),
    ("decompress-zlib", "urn:compress:zlib"),
];

/// Why the decompressors waive the three checks that parse their RDF faces.
const RDF_FACES: &str = "the RDF outputs are labels the caller puts on whatever the \
    container held (`as=`): the payload is not RDF, and no one fixture parses as Turtle, \
    N-Quads, N-Triples, TriG and JSON-LD at once. The label is tested by \
    `as_labels_the_payload_and_is_checked_for_shape`.";

/// `hello`, compressed by `compressor` through `kernel`: a stream the matching
/// decompressor accepts, and not UTF-8.
fn compressed_hello(kernel: &Kernel, compressor: &str) -> Vec<u8> {
    let request = Request::new(Verb::Source, Iri::parse(compressor).expect("valid IRI"))
        .with_arg("content", ArgRef::Inline(b"hello".to_vec()));
    block_on(kernel.issue(request, &Capability::root()))
        .expect("compressed")
        .bytes
        .to_vec()
}

#[test]
fn conforms() {
    // One space, shared: the kernel resolves through it and BODIES invokes the
    // endpoints in it directly.
    let space = Arc::new(ikigai_compress::space());
    let kernel = Kernel::new(space.clone());

    let mut suite = Suite::new().body_space(space);
    // The compressors take any bytes, so `hello` is a valid call — and once the
    // suite can resolve them it can hold them to purity and cacheability.
    for id in ["compress-gzip", "compress-zlib"] {
        suite = suite
            .pure(id)
            .cacheable(id)
            .fixture(Fixture::new(id, Verb::Source).arg("content", "hello"));
    }
    // The decompressors take a stream, so the fixture is a real one, as bytes.
    for (id, compressor) in PAIRS {
        suite = suite
            .pure(id)
            .cacheable(id)
            .fixture(
                Fixture::new(id, Verb::Source)
                    .arg_bytes("content", compressed_hello(&kernel, compressor)),
            )
            .opt_out_check(id, Check::SkolemRdf, RDF_FACES)
            .opt_out_check(id, Check::Vocabulary, RDF_FACES)
            .opt_out_check(id, Check::Datatypes, RDF_FACES);
    }

    let report = suite
        .self_named_space("compress", ikigai_compress::space)
        .run_blocking(&kernel);
    // Clean, every endpoint walked and nothing unprobed: a
    // BODIES or BODY-INPUTS check that lost its wiring turns this red.
    report.assert_clean_with(
        &Expected::new()
            .endpoints(ENDPOINTS.len())
            .walked(ENDPOINTS)
            .nothing_unprobed(),
    );
    assert_eq!(
        ikigai_core::space_iri("compress").as_str(),
        ikigai_compress::SPACE_ID
    );

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
