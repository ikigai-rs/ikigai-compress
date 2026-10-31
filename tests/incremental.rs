use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

use async_trait::async_trait;
use futures::{executor::block_on, stream, StreamExt};
use ikigai_core::{
    ArgRef, Body, Bytes, Capability, Description, Endpoint, Error, Exact, Invocation, Iri, Kernel,
    ReprType, Representation, Request, Result, Verb,
};

struct Source {
    chunks: Vec<Bytes>,
    polls: Arc<AtomicUsize>,
    fail: bool,
}

#[async_trait]
impl Endpoint for Source {
    async fn invoke(&self, _: &Invocation<'_>) -> Result<Representation> {
        panic!("stream source must use the body API")
    }

    async fn invoke_body(&self, _: &Invocation<'_>) -> Result<Representation<Body>> {
        let mut chunks: Vec<Result<Bytes>> = self.chunks.iter().cloned().map(Ok).collect();
        if self.fail {
            chunks.push(Err(Error::Endpoint("upstream failed".into())));
        }
        let polls = self.polls.clone();
        Ok(Representation::from_body(
            ReprType::new("application/octet-stream"),
            Body::once(stream::iter(chunks).inspect(move |_| {
                polls.fetch_add(1, Ordering::SeqCst);
            })),
        ))
    }
}

fn source_kernel(chunks: Vec<Bytes>, fail: bool) -> (Kernel, Arc<AtomicUsize>) {
    let polls = Arc::new(AtomicUsize::new(0));
    let space = ikigai_compress::space().bind(
        Exact::new("urn:input"),
        Source {
            chunks,
            polls: polls.clone(),
            fail,
        },
    );
    (Kernel::new(Arc::new(space)), polls)
}

fn request(algorithm: &str, compress: bool) -> Request {
    Request::new(
        Verb::Source,
        Iri::parse(format!(
            "urn:{}:{algorithm}",
            if compress { "compress" } else { "decompress" }
        ))
        .unwrap(),
    )
    .with_arg(
        "content",
        ArgRef::Reference(Iri::parse("urn:input").unwrap()),
    )
}

fn encoded(algorithm: &str, data: &[u8], level: u32) -> Vec<u8> {
    if algorithm == "gzip" {
        let mut encoder = flate2::GzBuilder::new()
            .mtime(0)
            .operating_system(255)
            .write(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    } else {
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::new(level));
        encoder.write_all(data).unwrap();
        encoder.finish().unwrap()
    }
}

fn fragmented(data: &[u8], size: usize) -> Vec<Bytes> {
    // Empty chunks, including at both ends, must not terminate a stream.
    let mut chunks = vec![Bytes::new()];
    for part in data.chunks(size) {
        chunks.push(Bytes::copy_from_slice(part));
        chunks.push(Bytes::new());
    }
    chunks
}

#[test]
fn every_compression_transreptor_is_incremental() {
    let kernel = Kernel::new(Arc::new(ikigai_compress::space()));
    for media in [ikigai_compress::GZIP, ikigai_compress::ZLIB] {
        for (from, to) in [("text/plain", media), (media, "text/plain")] {
            let plan = kernel.select_transreptor(from, to).unwrap();
            assert!(plan[0].incremental, "{from} -> {to} must stream");
        }
    }
}

#[test]
fn streamed_input_is_lazy_and_produces_before_upstream_eof() {
    block_on(async {
        // Incompressible enough to force output before the second input chunk.
        let mut seed = 1u32;
        let payload: Vec<u8> = (0..256 * 1024)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed as u8
            })
            .collect();
        for algorithm in ["gzip", "zlib"] {
            let (kernel, polls) = source_kernel(
                vec![payload.clone().into(), Bytes::from_static(b"tail")],
                false,
            );
            let repr = kernel
                .issue_body(request(algorithm, true), &Capability::root())
                .await
                .unwrap();
            assert!(repr.bytes.is_one_shot());
            assert_eq!(
                polls.load(Ordering::SeqCst),
                0,
                "no input pulled during invocation"
            );
            let mut output = repr.bytes.into_stream();
            let first = output.next().await.unwrap().unwrap();
            assert!(!first.is_empty());
            if algorithm == "gzip" {
                output.next().await.unwrap().unwrap();
            }
            assert_eq!(polls.load(Ordering::SeqCst), 1, "output before EOF");
            drop(output);
            assert_eq!(
                polls.load(Ordering::SeqCst),
                1,
                "dropping output stops input"
            );

            let compressed = encoded(algorithm, &payload, 6);
            let (kernel, polls) = source_kernel(
                vec![compressed.into(), Bytes::from_static(b"unread")],
                false,
            );
            let repr = kernel
                .issue_body(request(algorithm, false), &Capability::root())
                .await
                .unwrap();
            assert_eq!(polls.load(Ordering::SeqCst), 0);
            let mut output = repr.bytes.into_stream();
            assert!(!output.next().await.unwrap().unwrap().is_empty());
            assert_eq!(polls.load(Ordering::SeqCst), 1);
        }
    });
}

#[test]
fn fragmented_inputs_interoperate_with_flate2_at_every_level() {
    block_on(async {
        let payload: Vec<u8> = (0..131_072).map(|n| (n * 31) as u8).collect();
        for algorithm in ["gzip", "zlib"] {
            for level in [0, 1, 6, 9] {
                for data in [&b""[..], &payload[..]] {
                    let (kernel, _) = source_kernel(fragmented(data, 17), false);
                    let req = request(algorithm, true)
                        .with_arg("level", ArgRef::Inline(level.to_string().into_bytes()));
                    let compressed = kernel
                        .issue_body(req, &Capability::root())
                        .await
                        .unwrap()
                        .bytes
                        .collect(1024 * 1024)
                        .await
                        .unwrap();
                    let mut decoded = Vec::new();
                    if algorithm == "gzip" {
                        flate2::read::MultiGzDecoder::new(&compressed[..])
                            .read_to_end(&mut decoded)
                            .unwrap();
                    } else {
                        flate2::read::ZlibDecoder::new(&compressed[..])
                            .read_to_end(&mut decoded)
                            .unwrap();
                    }
                    assert_eq!(decoded, data);
                    // Chunking does not introduce sync-flush markers or change archive identity.
                    assert_eq!(&compressed[..], encoded(algorithm, data, level));

                    let (kernel, _) = source_kernel(fragmented(&compressed, 1), false);
                    let decoded = kernel
                        .issue_body(request(algorithm, false), &Capability::root())
                        .await
                        .unwrap()
                        .bytes
                        .collect(1024 * 1024)
                        .await
                        .unwrap();
                    assert_eq!(&decoded[..], data);
                }
            }
        }
    });
}

#[test]
fn cumulative_bound_refuses_without_draining_the_source() {
    block_on(async {
        for algorithm in ["gzip", "zlib"] {
            let compressed = encoded(algorithm, &vec![0; 8 * 1024 * 1024], 6);
            let (kernel, polls) = source_kernel(
                vec![compressed.into(), Bytes::from_static(b"unread")],
                false,
            );
            let req =
                request(algorithm, false).with_arg("max-bytes", ArgRef::Inline(b"1024".to_vec()));
            let body = kernel
                .issue_body(req, &Capability::root())
                .await
                .unwrap()
                .bytes;
            let err = body.collect(1024 * 1024).await.unwrap_err();
            assert!(matches!(err, Error::InvalidArgument { ref name, .. } if name == "content"));
            assert!(err.to_string().contains("1024-byte limit"), "{err}");
            assert_eq!(polls.load(Ordering::SeqCst), 1);

            for (data, limit, fits) in [
                (&b""[..], 0, true),
                (b"x", 0, false),
                (b"abc", 3, true),
                (b"abcd", 3, false),
            ] {
                let (kernel, _) = source_kernel(fragmented(&encoded(algorithm, data, 6), 1), false);
                let req = request(algorithm, false)
                    .with_arg("max-bytes", ArgRef::Inline(limit.to_string().into_bytes()));
                let result = kernel
                    .issue_body(req, &Capability::root())
                    .await
                    .unwrap()
                    .bytes
                    .collect(1024)
                    .await;
                assert_eq!(result.is_ok(), fits, "{algorithm}: {data:?}, limit {limit}");
            }
        }
    });
}

#[test]
fn truncation_checksums_and_upstream_errors_are_terminal_failures() {
    block_on(async {
        for algorithm in ["gzip", "zlib"] {
            let valid = encoded(algorithm, b"some payload", 6);
            for cut in 0..valid.len() {
                let (kernel, _) = source_kernel(fragmented(&valid[..cut], 1), false);
                let body = kernel
                    .issue_body(request(algorithm, false), &Capability::root())
                    .await
                    .unwrap()
                    .bytes;
                assert!(
                    body.collect(1024).await.is_err(),
                    "{algorithm} accepted truncation at {cut}"
                );
            }
            let mut corrupt = valid.clone();
            let last = corrupt.len() - 1;
            corrupt[last] ^= 1;
            let (kernel, _) = source_kernel(fragmented(&corrupt, 1), false);
            assert!(kernel
                .issue_body(request(algorithm, false), &Capability::root())
                .await
                .unwrap()
                .bytes
                .collect(1024)
                .await
                .is_err());

            for compress in [true, false] {
                // The error arrives after enough data to yield output, and must
                // still invalidate a collect rather than return a short success.
                let data = if compress {
                    vec![42; 128 * 1024]
                } else {
                    valid[..valid.len() - 1].to_vec()
                };
                let (kernel, _) = source_kernel(fragmented(&data, 7), true);
                let err = kernel
                    .issue_body(request(algorithm, compress), &Capability::root())
                    .await
                    .unwrap()
                    .bytes
                    .collect(1024 * 1024)
                    .await
                    .unwrap_err();
                assert!(err.to_string().contains("upstream failed"), "{err}");
            }
        }
    });
}

#[test]
fn concatenated_gzip_members_share_one_limit_and_validate_trailing_bytes() {
    block_on(async {
        let mut joined = encoded("gzip", b"first", 6);
        joined.extend(encoded("gzip", b"second", 6));
        for (limit, fits) in [(11, true), (10, false)] {
            let (kernel, _) = source_kernel(fragmented(&joined, 1), false);
            let req = request("gzip", false)
                .with_arg("max-bytes", ArgRef::Inline(limit.to_string().into_bytes()));
            let result = kernel
                .issue_body(req, &Capability::root())
                .await
                .unwrap()
                .bytes
                .collect(1024)
                .await;
            assert_eq!(result.is_ok(), fits);
            if fits {
                assert_eq!(&result.unwrap()[..], b"firstsecond");
            }
        }
        joined.extend(b"trailing");
        let (kernel, _) = source_kernel(fragmented(&joined, 1), false);
        assert!(kernel
            .issue_body(request("gzip", false), &Capability::root())
            .await
            .unwrap()
            .bytes
            .collect(1024)
            .await
            .is_err());
    });
}

#[test]
fn gzip_optional_fields_and_header_crc_can_cross_every_chunk_boundary() {
    block_on(async {
        let base = encoded("gzip", b"payload", 6);
        let mut header = base[..10].to_vec();
        header[3] = 2 | 4 | 8 | 16;
        header.extend(b"\x03\0abcname\0comment\0");
        let mut crc = flate2::Crc::new();
        crc.update(&header);
        header.extend((crc.sum() as u16).to_le_bytes());
        let mut valid = header.clone();
        valid.extend(&base[10..]);
        for size in 1..=valid.len() {
            let (kernel, _) = source_kernel(fragmented(&valid, size), false);
            assert_eq!(
                &kernel
                    .issue_body(request("gzip", false), &Capability::root())
                    .await
                    .unwrap()
                    .bytes
                    .collect(1024)
                    .await
                    .unwrap()[..],
                b"payload"
            );
        }
        valid[header.len() - 1] ^= 1;
        let (kernel, _) = source_kernel(fragmented(&valid, 1), false);
        assert!(kernel
            .issue_body(request("gzip", false), &Capability::root())
            .await
            .unwrap()
            .bytes
            .collect(1024)
            .await
            .is_err());
    });
}

// Exercise Invocation::transrept's attached-body path, not just references.
struct RoundTrip;
#[async_trait]
impl Endpoint for RoundTrip {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        self.invoke_body(inv)
            .await?
            .collect_within(inv.memory_budget().unwrap())
            .await
    }
    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        let body = Representation::from_body(
            ReprType::new("application/octet-stream"),
            Body::once(stream::iter(
                fragmented(b"attached stream", 1).into_iter().map(Ok),
            )),
        );
        let compress = inv
            .select_transreptor("application/octet-stream", ikigai_compress::GZIP)
            .unwrap();
        let compressed = inv.transrept(body, &compress).await?;
        assert!(compressed.bytes.is_one_shot());
        let decompress = inv
            .select_transreptor(ikigai_compress::GZIP, "text/plain")
            .unwrap();
        inv.transrept(compressed, &decompress).await
    }
    fn describe(&self) -> Description {
        Description::new("round-trip")
    }
}

#[test]
fn kernel_plans_pass_attached_streams_through_both_transforms() {
    let kernel = Kernel::new(Arc::new(
        ikigai_compress::space().bind(Exact::new("urn:round-trip"), RoundTrip),
    ));
    let req = Request::new(Verb::Source, Iri::parse("urn:round-trip").unwrap());
    let repr = block_on(kernel.issue(req, &Capability::root())).unwrap();
    assert_eq!(repr.bytes, b"attached stream");
    assert_eq!(repr.repr_type.media_type, "text/plain");
}

#[test]
fn streamed_output_can_exceed_the_collect_budget_but_bytes_api_refuses() {
    block_on(async {
        for algorithm in ["gzip", "zlib"] {
            let payload = vec![7; 128 * 1024];
            let compressed = encoded(algorithm, &payload, 6);
            let (kernel, _) = source_kernel(fragmented(&compressed, 3), false);
            let kernel = kernel.with_stream_memory(32, 32);
            let body = kernel
                .issue_body(request(algorithm, false), &Capability::root())
                .await
                .unwrap()
                .bytes;
            let mut chunks = body.into_stream();
            let mut seen = 0;
            while let Some(chunk) = chunks.next().await {
                let chunk = chunk.unwrap();
                assert!(chunk.len() <= 16 * 1024);
                assert!(chunk.iter().all(|b| *b == 7));
                seen += chunk.len();
            }
            assert_eq!(seen, payload.len());
            let err = kernel
                .issue(request(algorithm, false), &Capability::root())
                .await
                .unwrap_err();
            assert!(matches!(err, Error::OverBudget { .. }), "{err}");
        }
    });
}
