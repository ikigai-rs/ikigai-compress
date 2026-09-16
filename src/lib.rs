//! `ikigai-compress` — compression and decompression as resources.
//!
//! A standalone **ikigai module crate** (like `ikigai-text` or `ikigai-fn`): a
//! host links it in and mounts [`space`], rather than a host shipping the
//! behaviour itself. It depends only on the published `ikigai-core` kernel and a
//! pure-Rust backend, uses no OS or platform APIs, and compiles to
//! `wasm32-unknown-unknown`.
//!
//! ```text
//! source urn:file:ledger.nq | urn:compress:gzip | sink urn:file:ledger.nq.gz
//! source urn:file:ledger.nq.gz | urn:decompress:gzip as=application/n-quads
//! ```
//!
//! ## Two shapes, because compression has two
//!
//! 1. **Explicit endpoints** — `urn:compress:gzip`, `urn:decompress:gzip` (and
//!    the `zlib` pair) for a caller that *means* to compress.
//! 2. **Registered transreptors** — the same endpoints declare
//!    [`Description::transreptor`](ikigai_core::Description::transreptor), so the
//!    kernel's [`select_transreptor`](ikigai_core::Kernel::select_transreptor) can
//!    plan a conversion *through* them without anyone naming them. gzip is
//!    lossless and invertible, which is exactly what a transreptor is.
//!
//! ## Three properties this module is built around
//!
//! **The output is a function of the input.** gzip's header carries an mtime and
//! an OS byte, so the naive encoder produces different bytes for the same input
//! on every call. Both are pinned here (mtime `0`, OS `255` = unknown), so
//! `input → bytes` is a function: two backups of identical data are
//! byte-identical (diffable, dedup-able), and the `.cacheable()` declaration on
//! every endpoint is true rather than aspirational.
//! `compressing_the_same_input_twice_is_byte_identical` is the test that keeps it
//! that way, and `the_gzip_header_is_pinned_field_by_field` pins the exact header
//! bytes so a backend upgrade that changes them is a red test rather than a
//! silent change of every archive's identity.
//!
//! **The decompression bound refuses; it never truncates.** A few hundred bytes
//! of gzip can expand to gigabytes, and this module exists to be handed bytes
//! from elsewhere. `max-bytes` (default 64 MiB) caps the output, and exceeding it
//! is an error naming the limit — never a short, well-formed-looking
//! representation. The reader is capped at `max-bytes + 1`, so the refusal costs
//! one byte over the limit rather than the whole bomb. `max-bytes` itself is
//! bounded by [`MAX_BYTES_CEILING`] (1 GiB): a caller can lower the limit freely
//! and can raise it up to the ceiling, so a remote caller cannot name a number
//! that exhausts the host.
//!
//! **The piped value is the point.** Every endpoint declares its bytes input as
//! the single required argument `content`, which is both halves of pipeline
//! citizenship at once: the REPL engine fills *the one declared argument left
//! unnamed* (so `… | urn:compress:gzip` works with nothing named), and a kernel
//! transreption step passes its input as `content` by name. Every other argument
//! is optional — a second required argument would make the pipe ambiguous and
//! silently remove the module from every pipeline. `tests/pipeline.rs` runs the
//! real engine over this module to hold that.
//!
//! ## Media types
//!
//! Compression produces `application/gzip` / `application/zlib` (both registered,
//! RFC 6713) and says so in the representation's type. **Decompression cannot
//! recover the payload's media type** — neither container records one (gzip has a
//! filename field, not a type) — so the caller states it: `as=text/turtle`, with
//! `application/octet-stream` the default when nothing says otherwise. That is
//! also how the transreptor half works: a transreption step sets `as` to the
//! type it asked for.
//!
//! ## Capabilities: none, deliberately
//!
//! These endpoints declare no `requires` and enforce none. Compression is pure
//! computation over bytes the caller already holds: it reads no file, opens no
//! socket, and reveals nothing the caller could not compute itself, so a grant
//! would gate nothing. The one real hazard is resource exhaustion on
//! decompression, and the control for that is the bound above — a number that
//! applies to every caller — not an authority some callers hold.

use std::io::{Read, Write};

use flate2::read::{MultiGzDecoder, ZlibDecoder};
use flate2::write::ZlibEncoder;
use flate2::{Compression, GzBuilder};
use ikigai_core::{
    ArgSpec, Description, EndpointSpace, Error, Exact, FnEndpoint, Invocation, ReprType,
    Representation, Result, Verb,
};

// --- media types -----------------------------------------------------------

/// The registered media type of a gzip stream (RFC 6713).
pub const GZIP: &str = "application/gzip";
/// The registered media type of a zlib stream (RFC 6713).
pub const ZLIB: &str = "application/zlib";
/// The media type a decompressed payload carries when the caller says nothing —
/// the honest answer, since neither container records the payload's type.
pub const OCTET_STREAM: &str = "application/octet-stream";

/// The payload media types the **transreptor** half claims, in both directions.
///
/// This list is a declaration to the kernel's transreptor selection, not a limit
/// on the endpoints: `as=` accepts any media type, and `content` is always
/// arbitrary bytes. It exists because
/// [`Description::transreptor`](ikigai_core::Description::transreptor) takes
/// explicit media-type lists — there is no "any type" form — so a byte-level
/// transform has to enumerate the types it expects to be asked about. Adding one
/// here is the whole cost of making the kernel able to plan through it.
pub const PAYLOAD_TYPES: &[&str] = &[
    OCTET_STREAM,
    "text/plain",
    "text/turtle",
    "application/n-quads",
    "application/n-triples",
    "application/trig",
    "application/json",
    "application/ld+json",
    "application/xml",
    "text/csv",
];

// --- bounds ----------------------------------------------------------------

/// The default cap on decompressed output: 64 MiB.
pub const DEFAULT_MAX_BYTES: usize = 64 * 1024 * 1024;

/// The largest `max-bytes` a caller may ask for: 1 GiB.
///
/// `max-bytes` is a caller-supplied number, and this module is reachable over a
/// wire transport, so an unbounded one would just move the exhaustion from "a
/// bomb" to "a caller who typed a big number". Asking for more than this is
/// refused, naming the ceiling — the same rule as the bound it guards.
pub const MAX_BYTES_CEILING: usize = 1024 * 1024 * 1024;

/// The default deflate level (the `gzip(1)` default).
const DEFAULT_LEVEL: u32 = 6;

/// The gzip `OS` header byte this module writes: 255, "unknown".
///
/// Pinned rather than defaulted: the field's purpose is to record the producing
/// system, which is exactly the kind of ambient fact that makes the same input
/// compress to different bytes on different machines.
const OS_UNKNOWN: u8 = 255;

/// The gzip `MTIME` header field this module writes: 0, "no timestamp
/// available". The other half of determinism.
const MTIME_NONE: u32 = 0;

/// The XSD `string` datatype IRI.
///
/// ⚠ It is the `class` of `content` for want of anything better: `content` is
/// **arbitrary bytes**, usually not UTF-8 at all on the decompression side, and
/// the ArgSpec vocabulary has no term for opaque bytes. `xsd:base64Binary` would
/// be worse — it would tell a JSON-speaking client to base64-encode, which this
/// endpoint does not decode. The summary on each spec says what the value really
/// is.
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
/// The XSD `integer` datatype IRI — the `class` of `level` and `max-bytes`.
const XSD_INTEGER: &str = "http://www.w3.org/2001/XMLSchema#integer";

// --- argument reading ------------------------------------------------------

/// The bytes to (de)compress: the `content` argument, which the engine fills
/// from the pipe and a transreption step passes by name.
fn content<'a>(inv: &'a Invocation<'_>) -> Result<&'a [u8]> {
    inv.inline_arg("content")
}

/// The deflate level: absent → [`DEFAULT_LEVEL`]; otherwise `0..=9`, refusing
/// anything else rather than clamping (a clamped level would silently change the
/// output bytes of every later call).
fn level(inv: &Invocation<'_>) -> Result<u32> {
    let Ok(raw) = inv.inline_str("level") else {
        return Ok(DEFAULT_LEVEL);
    };
    match raw.trim().parse::<u32>() {
        Ok(n) if n <= 9 => Ok(n),
        _ => Err(Error::InvalidArgument {
            name: "level".to_string(),
            detail: format!("expected an integer 0..=9, got {:?}", raw.trim()),
        }),
    }
}

/// The output cap: absent → [`DEFAULT_MAX_BYTES`]; otherwise the parsed value,
/// refused above [`MAX_BYTES_CEILING`].
fn max_bytes(inv: &Invocation<'_>) -> Result<usize> {
    let Ok(raw) = inv.inline_str("max-bytes") else {
        return Ok(DEFAULT_MAX_BYTES);
    };
    let raw = raw.trim();
    let Ok(n) = raw.parse::<usize>() else {
        return Err(Error::InvalidArgument {
            name: "max-bytes".to_string(),
            detail: format!("expected a non-negative integer, got {raw:?}"),
        });
    };
    if n > MAX_BYTES_CEILING {
        return Err(Error::InvalidArgument {
            name: "max-bytes".to_string(),
            detail: format!(
                "{n} exceeds this module's ceiling of {MAX_BYTES_CEILING} bytes; \
                 decompress in pieces rather than asking the host for more"
            ),
        });
    }
    Ok(n)
}

/// The media type to label the decompressed payload with: the `as` argument if
/// present, else [`OCTET_STREAM`].
///
/// `as` is checked for the *shape* of a media type only (`type/subtype`, no
/// whitespace or control characters). Whether the payload really is that type is
/// the caller's claim — the container does not record one, so nothing here can
/// check it.
fn output_type(inv: &Invocation<'_>) -> Result<ReprType> {
    let Ok(raw) = inv.inline_str("as") else {
        return Ok(ReprType::new(OCTET_STREAM));
    };
    let media = raw.trim();
    let plausible = match media.split_once('/') {
        Some((kind, subtype)) => {
            !kind.is_empty()
                && !subtype.is_empty()
                && !subtype.contains('/')
                && !media.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    };
    if !plausible {
        return Err(Error::InvalidArgument {
            name: "as".to_string(),
            detail: format!("expected a media type like `text/turtle`, got {media:?}"),
        });
    }
    Ok(ReprType::new(media))
}

// --- the transforms --------------------------------------------------------

/// gzip `data` with a header pinned to constants — see [`OS_UNKNOWN`] and
/// [`MTIME_NONE`].
fn gzip(data: &[u8], level: u32) -> Result<Vec<u8>> {
    let mut encoder = GzBuilder::new()
        .mtime(MTIME_NONE)
        .operating_system(OS_UNKNOWN)
        .write(Vec::new(), Compression::new(level));
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .map_err(|e| Error::Endpoint(format!("gzip: {e}")))
}

/// zlib-compress `data`. The zlib header is two bytes derived from the level, so
/// there is no ambient field to pin.
fn zlib(data: &[u8], level: u32) -> Result<Vec<u8>> {
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(level));
    encoder
        .write_all(data)
        .and_then(|()| encoder.finish())
        .map_err(|e| Error::Endpoint(format!("zlib: {e}")))
}

/// Read `reader` to the end, **refusing** past `limit` rather than truncating.
///
/// The reader is capped at `limit + 1` bytes: enough to know the limit was
/// exceeded, never enough for the bomb to land. The one extra byte is why this
/// cannot be written as "read it all, then check the length".
fn bounded(reader: impl Read, limit: usize, algorithm: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let read = reader.take(limit as u64 + 1).read_to_end(&mut out);
    // A decode error and an over-limit read are both properties of `content`, so
    // both blame it — but they say different things, and the caller can tell the
    // difference from the detail.
    read.map_err(|e| Error::InvalidArgument {
        name: "content".to_string(),
        detail: format!("not a valid {algorithm} stream: {e}"),
    })?;
    if out.len() > limit {
        return Err(Error::InvalidArgument {
            name: "content".to_string(),
            detail: format!(
                "decompressed output exceeds the {limit}-byte limit set by `max-bytes`; \
                 refusing rather than returning a truncated representation \
                 (raise `max-bytes`, up to {MAX_BYTES_CEILING})"
            ),
        });
    }
    Ok(out)
}

// --- descriptions ----------------------------------------------------------

/// The `content` ArgSpec — required, unnamed in a pipe, and the name a kernel
/// transreption step uses. The single required argument on every endpoint here.
fn content_spec(what: &str) -> ArgSpec {
    ArgSpec::new("content")
        .summary(format!(
            "{what} — arbitrary bytes, not necessarily UTF-8; filled from the pipe"
        ))
        .class(XSD_STRING)
}

/// The shared description of a compressing endpoint.
fn compress_description(id: &str, algorithm: &str, media: &str) -> Description {
    Description::new(id)
        .title(format!("{algorithm} compress"))
        .summary(format!(
            "Compresses the piped `content` and returns a `{media}` representation. The \
             header is pinned (gzip's mtime and OS byte are fixed), so the same input and \
             `level` always produce the same bytes — identical data compresses to identical \
             archives. Also registered as a transreptor to `{media}`, so the kernel can \
             plan a conversion through it."
        ))
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .input(content_spec("the bytes to compress"))
        .input(
            ArgSpec::new("level")
                .summary("deflate level, 0 (store) to 9 (smallest)")
                .class(XSD_INTEGER)
                .default_value(DEFAULT_LEVEL.to_string()),
        )
        .output(media)
        .transreptor(PAYLOAD_TYPES.iter().copied(), [media])
}

/// The shared description of a decompressing endpoint.
fn decompress_description(id: &str, algorithm: &str, media: &str) -> Description {
    let mut description = Description::new(id)
        .title(format!("{algorithm} decompress"))
        .summary(format!(
            "Decompresses the piped `{media}` `content`. Output is capped at `max-bytes` \
             (default {DEFAULT_MAX_BYTES}, ceiling {MAX_BYTES_CEILING}) and a stream that \
             expands past it is REFUSED, never truncated. The payload's own media type \
             cannot be recovered from the container, so `as=` states it; without it the \
             result is `{OCTET_STREAM}`. Also registered as a transreptor from `{media}`, \
             which is how a transreption step's `as` reaches it."
        ))
        .verb(Verb::Source)
        .verb(Verb::Meta)
        .input(content_spec(
            format!("the {algorithm} stream to expand").as_str(),
        ))
        .input(
            ArgSpec::new("as")
                .summary(
                    "the media type to label the payload with — the caller's claim; the \
                     container records none",
                )
                .class(XSD_STRING)
                .default_value(OCTET_STREAM),
        )
        .input(
            ArgSpec::new("max-bytes")
                .summary("maximum decompressed size; exceeding it is an error, not a truncation")
                .class(XSD_INTEGER)
                .default_value(DEFAULT_MAX_BYTES.to_string()),
        );
    for payload in PAYLOAD_TYPES {
        description = description.output(*payload);
    }
    description.transreptor([media], PAYLOAD_TYPES.iter().copied())
}

// --- endpoints -------------------------------------------------------------

/// `urn:compress:gzip` — gzip the piped bytes (`application/gzip`).
pub fn compress_gzip() -> FnEndpoint {
    FnEndpoint::new("compress-gzip", |inv: &Invocation<'_>| {
        let bytes = gzip(content(inv)?, level(inv)?)?;
        Ok(Representation::new(ReprType::new(GZIP), bytes).cacheable())
    })
    .with_description(compress_description("compress-gzip", "gzip", GZIP))
}

/// `urn:decompress:gzip` — expand a gzip stream, bounded by `max-bytes`.
///
/// Concatenated members are read (the `gzip -c a b > ab.gz` case); trailing
/// bytes that are not another member are an error rather than silently ignored
/// input.
pub fn decompress_gzip() -> FnEndpoint {
    FnEndpoint::new("decompress-gzip", |inv: &Invocation<'_>| {
        let bytes = bounded(MultiGzDecoder::new(content(inv)?), max_bytes(inv)?, "gzip")?;
        Ok(Representation::new(output_type(inv)?, bytes).cacheable())
    })
    .with_description(decompress_description("decompress-gzip", "gzip", GZIP))
}

/// `urn:compress:zlib` — zlib-compress the piped bytes (`application/zlib`).
pub fn compress_zlib() -> FnEndpoint {
    FnEndpoint::new("compress-zlib", |inv: &Invocation<'_>| {
        let bytes = zlib(content(inv)?, level(inv)?)?;
        Ok(Representation::new(ReprType::new(ZLIB), bytes).cacheable())
    })
    .with_description(compress_description("compress-zlib", "zlib", ZLIB))
}

/// `urn:decompress:zlib` — expand a zlib stream, bounded by `max-bytes`.
pub fn decompress_zlib() -> FnEndpoint {
    FnEndpoint::new("decompress-zlib", |inv: &Invocation<'_>| {
        let bytes = bounded(ZlibDecoder::new(content(inv)?), max_bytes(inv)?, "zlib")?;
        Ok(Representation::new(output_type(inv)?, bytes).cacheable())
    })
    .with_description(decompress_description("decompress-zlib", "zlib", ZLIB))
}

/// The module's space: `urn:compress:*` and `urn:decompress:*`.
///
/// A host mounts this. The namespace is `urn:{verb}:{algorithm}`, so a second
/// algorithm is one more pair of bindings and one more line in
/// [`PAYLOAD_TYPES`]'s neighbours — never a reshuffle.
///
/// ```
/// # use std::sync::Arc;
/// # use futures::executor::block_on;
/// use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request, Verb};
///
/// let kernel = Kernel::new(Arc::new(ikigai_compress::space()));
/// let request = Request::new(Verb::Source, Iri::parse("urn:compress:gzip").unwrap())
///     .with_arg("content", ArgRef::Inline(b"hello hello hello".to_vec()));
/// let gz = block_on(kernel.issue(request, &Capability::root())).unwrap();
/// assert_eq!(gz.repr_type.media_type, "application/gzip");
/// assert_eq!(&gz.bytes[..2], b"\x1f\x8b");
/// ```
pub fn space() -> EndpointSpace {
    EndpointSpace::new()
        .bind(Exact::new("urn:compress:gzip"), compress_gzip())
        .bind(Exact::new("urn:decompress:gzip"), decompress_gzip())
        .bind(Exact::new("urn:compress:zlib"), compress_zlib())
        .bind(Exact::new("urn:decompress:zlib"), decompress_zlib())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::executor::block_on;
    use ikigai_core::{ArgRef, Capability, Iri, Kernel, Request};
    use std::sync::Arc;

    fn kernel() -> Kernel {
        Kernel::new(Arc::new(space()))
    }

    /// Resolve `iri` with the given inline arguments under root authority.
    fn source(iri: &str, args: &[(&str, &[u8])]) -> Result<Representation> {
        let mut request = Request::new(Verb::Source, Iri::parse(iri).expect("valid IRI"));
        for (name, value) in args {
            request = request.with_arg(*name, ArgRef::Inline(value.to_vec()));
        }
        block_on(kernel().issue(request, &Capability::root()))
    }

    /// The description of one bound endpoint.
    fn describe(endpoint: &FnEndpoint) -> Description {
        use ikigai_core::Endpoint;
        endpoint.describe()
    }

    fn arg(description: &Description, name: &str) -> ArgSpec {
        description
            .inputs
            .iter()
            .find(|input| input.name == name)
            .unwrap_or_else(|| panic!("{} declares `{name}`", description.id))
            .clone()
    }

    /// Bytes that are deliberately not UTF-8 — the case `xsd:string` misdescribes
    /// and every text-oriented module would corrupt.
    fn binary_payload() -> Vec<u8> {
        (0u16..=255).map(|b| b as u8).cycle().take(4096).collect()
    }

    // ---- round trips -------------------------------------------------------

    #[test]
    fn gzip_round_trips_arbitrary_bytes() {
        let payload = binary_payload();
        let gz = source("urn:compress:gzip", &[("content", &payload)]).expect("compressed");
        assert_eq!(gz.repr_type.media_type, GZIP);
        assert!(gz.bytes.len() < payload.len(), "cyclic bytes compress");

        let back = source("urn:decompress:gzip", &[("content", &gz.bytes)]).expect("decompressed");
        assert_eq!(back.bytes, payload);
        assert_eq!(back.repr_type.media_type, OCTET_STREAM);
    }

    #[test]
    fn zlib_round_trips_arbitrary_bytes() {
        let payload = binary_payload();
        let z = source("urn:compress:zlib", &[("content", &payload)]).expect("compressed");
        assert_eq!(z.repr_type.media_type, ZLIB);

        let back = source("urn:decompress:zlib", &[("content", &z.bytes)]).expect("decompressed");
        assert_eq!(back.bytes, payload);
    }

    #[test]
    fn an_empty_input_round_trips() {
        let gz = source("urn:compress:gzip", &[("content", b"")]).expect("compressed");
        let back = source("urn:decompress:gzip", &[("content", &gz.bytes)]).expect("decompressed");
        assert!(back.bytes.is_empty());
    }

    #[test]
    fn concatenated_gzip_members_all_decompress() {
        // `gzip -c a b > ab.gz` is one file of two members; a single-member
        // decoder would silently return only the first.
        let a = source("urn:compress:gzip", &[("content", b"first ")]).expect("compressed");
        let b = source("urn:compress:gzip", &[("content", b"second")]).expect("compressed");
        let joined: Vec<u8> = a.bytes.iter().chain(b.bytes.iter()).copied().collect();
        let back = source("urn:decompress:gzip", &[("content", &joined)]).expect("decompressed");
        assert_eq!(back.bytes, b"first second");
    }

    // ---- determinism -------------------------------------------------------

    #[test]
    fn compressing_the_same_input_twice_is_byte_identical() {
        // Without this, `.cacheable()` is a lie and two backups of identical data
        // stop being identical — the failure shows up months later as a mystery
        // diff, so it is pinned here on both algorithms.
        for iri in ["urn:compress:gzip", "urn:compress:zlib"] {
            let once = source(iri, &[("content", b"the same bytes")]).expect("compressed");
            let twice = source(iri, &[("content", b"the same bytes")]).expect("compressed");
            assert_eq!(once.bytes, twice.bytes, "{iri} is a function of its input");
        }
    }

    #[test]
    fn the_gzip_header_is_pinned_field_by_field() {
        // The shape of what leaves the process, asserted rather than described:
        // a backend upgrade that starts stamping an mtime or an OS byte changes
        // the identity of every archive this module has ever written, and this is
        // the only place that would notice.
        let gz = source("urn:compress:gzip", &[("content", b"x")]).expect("compressed");
        assert_eq!(&gz.bytes[0..2], b"\x1f\x8b", "gzip magic");
        assert_eq!(gz.bytes[2], 8, "deflate");
        assert_eq!(
            gz.bytes[3], 0,
            "no FLG bits — no name, comment or extra field"
        );
        assert_eq!(&gz.bytes[4..8], &[0, 0, 0, 0], "MTIME pinned to 0");
        assert_eq!(gz.bytes[9], OS_UNKNOWN, "OS pinned to 255 (unknown)");
    }

    #[test]
    fn the_level_changes_the_bytes_and_is_bounded() {
        let payload = binary_payload();
        let fast = source(
            "urn:compress:gzip",
            &[("content", &payload), ("level", b"1")],
        )
        .expect("compressed");
        let small = source(
            "urn:compress:gzip",
            &[("content", &payload), ("level", b"9")],
        )
        .expect("compressed");
        assert!(small.bytes.len() <= fast.bytes.len());
        // Both still decompress to the same payload.
        for repr in [&fast, &small] {
            let back =
                source("urn:decompress:gzip", &[("content", &repr.bytes)]).expect("decompressed");
            assert_eq!(back.bytes, payload);
        }

        let bad = source("urn:compress:gzip", &[("content", b"x"), ("level", b"12")])
            .expect_err("level 12 is refused, not clamped");
        assert!(matches!(bad, Error::InvalidArgument { ref name, .. } if name == "level"));
        assert!(bad.to_string().contains("0..=9"), "{bad}");
    }

    // ---- the bomb surface --------------------------------------------------

    /// 8 MiB of zeros — a few KiB compressed. The shape of a zip bomb, in
    /// miniature.
    fn bomb() -> Vec<u8> {
        let zeros = vec![0u8; 8 * 1024 * 1024];
        source("urn:compress:gzip", &[("content", &zeros)])
            .expect("compressed")
            .bytes
    }

    #[test]
    fn a_bomb_is_refused_naming_the_limit_not_truncated() {
        let bomb = bomb();
        assert!(bomb.len() < 64 * 1024, "the bomb is small: {}", bomb.len());

        let err = source(
            "urn:decompress:gzip",
            &[("content", &bomb), ("max-bytes", b"1024")],
        )
        .expect_err("a stream that expands past the limit is refused");
        let message = err.to_string();
        assert!(message.contains("1024"), "names the limit: {message}");
        assert!(message.contains("refusing"), "refuses: {message}");
        assert!(matches!(err, Error::InvalidArgument { .. }), "{err:?}");

        // And the same bytes are fine under a limit that fits them — the refusal
        // is the bound doing its job, not the decoder failing.
        let ok = source(
            "urn:decompress:gzip",
            &[("content", &bomb), ("max-bytes", b"8388608")],
        )
        .expect("fits");
        assert_eq!(ok.bytes.len(), 8 * 1024 * 1024);
    }

    #[test]
    fn the_default_limit_is_64_mib_and_applies_unasked() {
        // The bound is not opt-in: a caller who names no limit still gets one.
        let d = describe(&decompress_gzip());
        assert_eq!(
            arg(&d, "max-bytes").default.as_deref(),
            Some(DEFAULT_MAX_BYTES.to_string().as_str())
        );
        assert_eq!(DEFAULT_MAX_BYTES, 67_108_864);
    }

    #[test]
    fn max_bytes_is_itself_bounded_by_the_ceiling() {
        let err = source(
            "urn:decompress:gzip",
            &[("content", b"unused"), ("max-bytes", b"9999999999")],
        )
        .expect_err("a caller cannot name an unbounded limit");
        assert!(
            err.to_string().contains(&MAX_BYTES_CEILING.to_string()),
            "names the ceiling: {err}"
        );
        // Lowering is always fine.
        assert!(source(
            "urn:decompress:zlib",
            &[("content", b"unused"), ("max-bytes", b"0")]
        )
        .is_err());
    }

    #[test]
    fn corrupt_input_is_an_error_about_content() {
        let err = source("urn:decompress:gzip", &[("content", b"not a gzip stream")])
            .expect_err("garbage is refused");
        assert!(matches!(err, Error::InvalidArgument { ref name, .. } if name == "content"));
        assert!(err.to_string().contains("gzip"), "{err}");

        // Trailing garbage after a valid member is an error too, rather than a
        // silent partial read.
        let gz = source("urn:compress:gzip", &[("content", b"payload")]).expect("compressed");
        let mut tampered = gz.bytes.clone();
        tampered.extend_from_slice(b"trailing");
        assert!(source("urn:decompress:gzip", &[("content", &tampered)]).is_err());
    }

    #[test]
    fn a_missing_content_argument_is_a_missing_argument() {
        let err = source("urn:compress:gzip", &[]).expect_err("content is required");
        assert!(matches!(err, Error::MissingArgument(ref name) if name == "content"));
    }

    // ---- media types -------------------------------------------------------

    #[test]
    fn as_labels_the_payload_and_is_checked_for_shape() {
        let gz =
            source("urn:compress:gzip", &[("content", b"@prefix ik: <x> .")]).expect("compressed");
        let back = source(
            "urn:decompress:gzip",
            &[("content", &gz.bytes), ("as", b"text/turtle")],
        )
        .expect("decompressed");
        assert_eq!(back.repr_type.media_type, "text/turtle");

        let err = source(
            "urn:decompress:gzip",
            &[("content", &gz.bytes), ("as", b"turtle")],
        )
        .expect_err("`turtle` is not a media type");
        assert!(matches!(err, Error::InvalidArgument { ref name, .. } if name == "as"));
    }

    // ---- the transreptor half ---------------------------------------------

    #[test]
    fn the_kernel_can_select_these_transreptors_by_media_type() {
        // The half that makes this module ROC-shaped rather than a utility crate:
        // nobody names the endpoint, the kernel plans through it.
        let kernel = kernel();
        let to_gzip = kernel
            .select_transreptor("text/turtle", GZIP)
            .expect("turtle → gzip is plannable");
        assert_eq!(to_gzip.len(), 1);
        assert_eq!(to_gzip[0].endpoint, "urn:compress:gzip");
        assert_eq!(to_gzip[0].to, GZIP);

        let from_gzip = kernel
            .select_transreptor(GZIP, "text/turtle")
            .expect("gzip → turtle is plannable");
        assert_eq!(from_gzip[0].endpoint, "urn:decompress:gzip");
        // ★ and the step sets `as`, which is exactly the argument the endpoint
        // reads to label the payload — the two halves meet here.
        assert_eq!(from_gzip[0].to, "text/turtle");

        assert_eq!(
            kernel.select_transreptor("text/plain", ZLIB).expect("zlib")[0].endpoint,
            "urn:compress:zlib"
        );
    }

    #[test]
    fn every_endpoint_is_auto_invocable() {
        // A transreptor the kernel cannot drive with just `content` + `as` is a
        // transreptor for discovery only. `content` being the sole required
        // argument is what makes these selectable — the same property that makes
        // them pipeline citizens.
        for endpoint in [
            compress_gzip(),
            decompress_gzip(),
            compress_zlib(),
            decompress_zlib(),
        ] {
            let d = describe(&endpoint);
            assert!(
                ikigai_core::is_auto_invocable(&d),
                "{} is auto-invocable",
                d.id
            );
            let required: Vec<&str> = d
                .inputs
                .iter()
                .filter(|i| i.required)
                .map(|i| i.name.as_str())
                .collect();
            assert_eq!(required, ["content"], "{} requires only content", d.id);
        }
    }

    // ---- the declared contract --------------------------------------------

    #[test]
    fn every_endpoint_declares_a_typed_contract_and_no_capability() {
        for endpoint in [
            compress_gzip(),
            decompress_gzip(),
            compress_zlib(),
            decompress_zlib(),
        ] {
            let d = describe(&endpoint);
            assert!(d.verbs.contains(&Verb::Source), "{} sources", d.id);
            assert!(d.verbs.contains(&Verb::Meta), "{} self-describes", d.id);
            assert!(!d.summary.is_empty(), "{} has a summary", d.id);
            for input in &d.inputs {
                assert!(input.class.is_some(), "{}.{} has a class", d.id, input.name);
                assert!(
                    !input.summary.is_empty(),
                    "{}.{} has a summary",
                    d.id,
                    input.name
                );
            }
            // Declared = enforced, in the direction that can be checked here:
            // nothing is declared, and nothing is enforced (every test above runs
            // under `Capability::root()`, but `space_resolves_without_a_grant`
            // holds the other end).
            assert!(d.requires.is_empty(), "{} needs no capability", d.id);
            assert!(d.transreption().is_some(), "{} is a transreptor", d.id);
            assert!(!d.outputs.is_empty(), "{} declares its outputs", d.id);
        }
    }

    #[test]
    fn the_endpoints_resolve_without_any_grant() {
        // The capability argument of the recipe, as a test: an empty capability
        // is enough, because these endpoints gate nothing.
        let request = Request::new(Verb::Source, Iri::parse("urn:compress:gzip").unwrap())
            .with_arg("content", ArgRef::Inline(b"hello".to_vec()));
        let nothing = Capability::root().attenuate(Vec::<String>::new());
        assert!(block_on(kernel().issue(request, &nothing)).is_ok());
    }

    #[test]
    fn the_compressors_claim_every_payload_type_and_produce_exactly_one() {
        // The transreptor declaration is a CROSS PRODUCT (`from` × `to`), so a
        // multi-element `to` on a compressor would claim conversions it cannot
        // do. One `to` per compressing endpoint keeps every claimed pair true.
        for (endpoint, media) in [(compress_gzip(), GZIP), (compress_zlib(), ZLIB)] {
            let d = describe(&endpoint);
            let t = d.transreption().expect("transreptor");
            assert_eq!(t.to, vec![media.to_string()]);
            assert_eq!(t.from.len(), PAYLOAD_TYPES.len());
        }
        for (endpoint, media) in [(decompress_gzip(), GZIP), (decompress_zlib(), ZLIB)] {
            let d = describe(&endpoint);
            let t = d.transreption().expect("transreptor");
            assert_eq!(t.from, vec![media.to_string()]);
            assert_eq!(t.to.len(), PAYLOAD_TYPES.len());
        }
    }

    #[test]
    fn the_space_binds_both_namespaces() {
        // `urn:{verb}:{algorithm}` — the shape that makes a third algorithm two
        // more bindings rather than a redesign. (Listed rather than resolved as
        // `Meta`: rendering a description needs a host's `MetaRenderer`, which a
        // bare `Kernel::new` has none of.)
        use ikigai_core::Space;
        let bound: Vec<String> = space()
            .entries()
            .expect("entries")
            .into_iter()
            .map(|entry| entry.pattern)
            .collect();
        for iri in [
            "urn:compress:gzip",
            "urn:decompress:gzip",
            "urn:compress:zlib",
            "urn:decompress:zlib",
        ] {
            assert!(bound.iter().any(|p| p == iri), "{iri} is bound: {bound:?}");
        }
        assert_eq!(bound.len(), 4, "and nothing else: {bound:?}");
    }

    #[test]
    fn results_are_marked_cacheable() {
        let gz = source("urn:compress:gzip", &[("content", b"hello")]).expect("compressed");
        assert!(
            !matches!(gz.expiry, ikigai_core::Expiry::Always),
            "a pure function of its input is cacheable"
        );
    }
}
