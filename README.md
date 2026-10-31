# ikigai-compress

Compression and decompression as **resources** for
[ikigai](https://github.com/ikigai-rs) — `urn:compress:*` and `urn:decompress:*`.

```text
source urn:file:ledger.nq | urn:compress:gzip | sink urn:file:ledger.nq.gz
source urn:file:ledger.nq.gz | urn:decompress:gzip as=application/n-quads
```

Nothing in this ecosystem compressed anything before this crate. That was found
while specifying "store the backups compressed", which turned out to have no
primitive behind it.

Like `ikigai-text`, this is a standalone **module crate**: a host links it in and
mounts [`space`], rather than a host shipping the behaviour itself. It depends
only on the published `ikigai-core` kernel and a pure-Rust backend
(`flate2`/`miniz_oxide` — no C toolchain, no `cc`), uses no OS or platform APIs,
and compiles to `wasm32-unknown-unknown`.

The space names itself `urn:iki:space:compress` (`ikigai_compress::SPACE_ID`).

## Two shapes, because compression has two

**Explicit endpoints**, for a caller that means to compress:

| IRI | args | result |
|-----|------|--------|
| `urn:compress:gzip` | `content` (piped, required); `level=` 0–9 (default 6) | `application/gzip` |
| `urn:decompress:gzip` | `content` (piped, required); `as=` media type (default `application/octet-stream`); `max-bytes=` (default 67108864) | the payload, labelled `as` |
| `urn:compress:zlib` | as above | `application/zlib` |
| `urn:decompress:zlib` | as above | the payload, labelled `as` |

**Registered transreptors**, for the kernel: each endpoint declares its
conversion, so `Kernel::select_transreptor` can plan *through* it without anyone
naming it. gzip is lossless and invertible, which is exactly what a transreptor
is — and it means "store it compressed" can become a property of writing a
representation rather than a function someone remembers to call.

```rust
let kernel = Kernel::new(Arc::new(ikigai_compress::space()));
let plan = kernel.select_transreptor("application/gzip", "text/turtle").unwrap();
assert_eq!(plan[0].endpoint, "urn:decompress:gzip");   // nobody named it
```

## Incremental bodies

All four transreptors declare `incremental`. `content` accepts inline bytes,
resource references, stored content IDs and attached streams. Streamed input
flows through flate2's incremental codecs a chunk at a time, producing one-shot
output in chunks of at most 16 KiB. Gzip framing and checksums belong to flate2's
push writers; zlib decoding uses `Decompress` to check stream completion. The
transform holds the current input chunk, codec state and bounded output buffers;
it never collects a streamed input.
Dropping the output stops reading upstream. Buffered input retains buffered,
cacheable output; the bytes API collects under the run's memory budget.

`max-bytes` counts decompressed output across all chunks and all concatenated
gzip members. Truncation, invalid checksums and upstream failures end the body
with an error, even if earlier chunks have already been delivered. Consumers
must read through the end and honor that error. Gzip optional header fields and
header CRCs are handled by flate2.

Version 0.4.0 changes the endpoint constructors to return `CompressionEndpoint`,
which implements `Endpoint`, in place of `FnEndpoint`.

## Three properties it is built around

**The output is a function of the input.** gzip's header carries an mtime and an
OS byte, so the naive encoder produces different bytes for the same input every
time — which would make `.cacheable()` a lie and stop two backups of identical
data from being identical. Both fields are pinned (mtime `0`, OS `255`), there is
a test that compresses the same input twice and asserts equality, and another
that pins the header byte by byte so a backend upgrade that starts stamping
something is a red test rather than a silent change to every archive's identity.

**The decompression bound refuses; it never truncates.** A few hundred bytes of
gzip can expand to gigabytes, and this module exists to be handed bytes from
elsewhere. `max-bytes` (default 64 MiB) caps the output and exceeding it is an
error *naming the limit* — never a short, well-formed-looking representation that
passes every shape check downstream. The output sink refuses writes beyond the
limit, and codec output buffers stay fixed in size. `max-bytes` itself is capped
at 1 GiB: a caller can lower it freely and raise it to the
ceiling, so a remote caller cannot name a number that exhausts the host.

**The piped value is the point.** Every endpoint declares its bytes input as the
single required argument `content`, which is both halves of pipeline citizenship
at once: the REPL engine fills *the one declared argument left unnamed*, and a
kernel transreption step passes its input as `content` by name. Every other
argument is optional — a second required one would make the pipe ambiguous, and
an endpoint with no required by-value input cannot be piped into at all.
`tests/pipeline.rs` drives the **real engine** over this module, because that
rule lives in the engine: a kernel-level test names `content` itself and cannot
see it.

## Media types

Compression produces `application/gzip` / `application/zlib` (both registered,
RFC 6713) and says so. **Decompression cannot recover the payload's media
type** — neither container records one (gzip has a filename field, not a type) —
so the caller states it with `as=`, defaulting to `application/octet-stream`.
That is also the mechanism behind the transreptor half: a transreption step sets
`as` to the type it asked for.

Algorithms ship here when they have **a registered media type and a pure-Rust
implementation**: the transreptor half is media-type-addressed, so an algorithm
with no type has nothing to be selected by. That is why `zstd` (pure-Rust
compressor does not exist; the crate is a C binding) and raw `deflate` (no
registered media type — use `zlib`) are absent.

## Capabilities: none, deliberately

These endpoints declare no `requires` and enforce none. Compression is pure
computation over bytes the caller already holds: it reads no file, opens no
socket, and reveals nothing the caller could not compute itself, so a grant would
gate nothing — and a declared-but-unenforced capability would make the action
manifold lie. The one real hazard is resource exhaustion on decompression, and
the control for that is the bound above: a number that applies to every caller,
not an authority some callers hold.

## Conformance

`tests/conformance.rs` runs
[`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance) over the
whole space, `BODIES` and `BODY-INPUTS` included (conformance 0.12). The
compressors pass every check with no waiver. The decompressors get a binary
fixture (`Fixture::arg_bytes`: the gzip/zlib stream of `hello`), so every
invoking check runs on them too, except the three that parse each declared RDF
face (`SKOLEM-RDF`, `VOCABULARY`, `DATATYPES`): a decompressor's RDF outputs
are only the caller's `as=` label on whatever the container held, and no one
payload parses as Turtle, N-Quads, N-Triples, TriG and JSON-LD at once. The
suite gets the kernel's own space through `Suite::body_space` and the shared
content store through `Suite::body_input_store`, comparing all four body input
transports. The test requires that nothing is left unprobed. `tests/incremental.rs`
checks streaming, bounds and malformed containers; `tests/pipeline.rs` and the
unit tests cover the endpoints end to end.

## Accessibility

What people read from this crate is its refusals and its endpoints' descriptions; the bytes it
answers are not meant to be read. [ACCESSIBILITY.md](ACCESSIBILITY.md) says what that text is,
the rules it follows, what is not tested, and how to report a barrier.

## Licence

MIT OR Apache-2.0.
