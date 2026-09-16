//! Pipeline citizenship, through the **engine** — the only place it is visible.
//!
//! `source urn:file:x | urn:compress:gzip` is the shape every caller reaches for,
//! and whether it works is decided by a rule that lives in `ikigai-engine`, not in
//! the kernel: a `Source` fills **the one declared argument left unnamed**, and an
//! endpoint with no required by-value input cannot be piped into at all. A
//! kernel-level test names `content` explicitly and therefore cannot see any of
//! this; neither can the conformance suite, which also calls the kernel. So this
//! file drives the real engine over the real module.
//!
//! ⚠ The engine fetches each endpoint's contract as `Meta as=application/json`,
//! and when that fails it fails **open**, routing every value to an input named
//! `in`. On a kernel with no JSON meta renderer every pipe "works" and a routing
//! defect is invisible — so the kernel here is built with
//! `Kernel::with_meta_renderer`, and the first test below is the witness that the
//! contract is really being read.

use std::sync::Arc;

use futures::executor::block_on;
use ikigai_core::{Description, Kernel, MetaRenderer, ReprType, Representation};
use ikigai_engine::{Action, Engine};

/// The contract renderer the engine actually asks for. Without it these tests
/// would pass over a module whose arguments are named wrongly.
struct JsonRenderer;

impl MetaRenderer for JsonRenderer {
    fn render(
        &self,
        description: &Description,
        _target: &ReprType,
    ) -> ikigai_core::Result<Representation> {
        Ok(Representation::new(
            ReprType::new("application/json"),
            serde_json::to_vec(description).expect("serialize description"),
        ))
    }
}

fn engine() -> Engine {
    Engine::new(Kernel::with_meta_renderer(
        Arc::new(ikigai_compress::space()),
        Arc::new(JsonRenderer),
    ))
}

fn run(line: &str) -> Result<String, String> {
    match block_on(engine().eval_async(line)) {
        Action::Output(entry) => entry.result,
        _ => Err(format!("`{line}` produced no output")),
    }
}

#[test]
fn a_pipe_round_trips_through_compress_and_decompress() {
    // The headline shape, with nothing named: the value flows into `content` at
    // both stages because it is the one required argument each declares.
    let out =
        run("source urn:compress:gzip the quick brown fox | urn:decompress:gzip as=text/plain")
            .expect("round trip");
    assert_eq!(out.trim_end(), "the quick brown fox");
}

#[test]
fn the_zlib_pair_pipes_the_same_way() {
    let out = run("source urn:compress:zlib hello zlib | urn:decompress:zlib as=text/plain")
        .expect("round trip");
    assert_eq!(out.trim_end(), "hello zlib");
}

#[test]
fn a_named_argument_rides_alongside_the_piped_value() {
    // `level` is named, so it does not compete for the pipe: the value still
    // routes to `content`. This is the case that breaks when a second argument is
    // made required.
    let out =
        run("source urn:compress:gzip compress me level=9 | urn:decompress:gzip as=text/plain")
            .expect("round trip at level 9");
    assert_eq!(out.trim_end(), "compress me");
}

#[test]
fn the_bound_refuses_through_the_engine_too() {
    // The refusal is an error the caller sees, not a short result they do not.
    let err =
        run("source urn:compress:gzip aaaaaaaaaaaaaaaaaaaa | urn:decompress:gzip max-bytes=4")
            .expect_err("a payload over the limit is refused");
    assert!(err.contains('4'), "names the limit: {err}");
    assert!(err.contains("refusing"), "{err}");
}

#[test]
fn the_tests_above_would_notice_a_routing_defect() {
    // The witness for the ⚠ at the top of this file, and the reason these tests
    // are worth anything. On a kernel that cannot answer `Meta as=application/json`
    // the engine fails OPEN: it routes the value to an input named `in`, which
    // this module does not declare. If that were the kernel under test, every
    // test above would be passing over a module whose argument could be named
    // anything at all.
    //
    // So: the same line, on a kernel with no meta renderer, must FAIL — and fail
    // by not finding `content`.
    let blind = Engine::new(Kernel::new(Arc::new(ikigai_compress::space())));
    let err = match block_on(blind.eval_async("source urn:compress:gzip hello")) {
        Action::Output(entry) => entry
            .result
            .expect_err("a blind engine cannot route the value"),
        _ => panic!("expected output"),
    };
    assert!(
        err.contains("content"),
        "the value never reached content: {err}"
    );
}
