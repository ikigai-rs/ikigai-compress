//! Reproductions and coverage from the gzip correctness audit.
use futures::executor::block_on;
use ikigai_core::{ArgRef, Capability, Error, Iri, Kernel, Representation, Request, Result, Verb};
use std::sync::Arc;

fn source(iri: &str, content: &[u8], option: Option<(&str, &[u8])>) -> Result<Representation> {
    let kernel = Kernel::new(Arc::new(ikigai_compress::space()));
    let mut request = Request::new(Verb::Source, Iri::parse(iri).unwrap())
        .with_arg("content", ArgRef::Inline(content.to_vec()));
    if let Some((name, value)) = option {
        request = request.with_arg(name, ArgRef::Inline(value.to_vec()));
    }
    block_on(kernel.issue(request, &Capability::root()))
}

fn assert_invalid_utf8_is_refused(option: &str) {
    for algorithm in ["gzip", "zlib"] {
        let compress = format!("urn:compress:{algorithm}");
        let encoded = source(&compress, b"payload", None).unwrap();
        let endpoint = if option == "level" {
            compress
        } else {
            format!("urn:decompress:{algorithm}")
        };
        let content = if option == "level" {
            &b"payload"[..]
        } else {
            &encoded.bytes[..]
        };
        let result = source(&endpoint, content, Some((option, b"\xff")));
        assert!(
            matches!(result, Err(Error::InvalidArgument { ref name, .. }) if name == option),
            "{endpoint} must refuse invalid UTF-8 in {option}, got {result:?}"
        );
    }
}

#[test]
fn invalid_utf8_level_is_refused_instead_of_defaulted() {
    assert_invalid_utf8_is_refused("level");
}

#[test]
fn invalid_utf8_max_bytes_is_refused_instead_of_defaulted() {
    assert_invalid_utf8_is_refused("max-bytes");
}

#[test]
fn invalid_utf8_media_type_is_refused_instead_of_defaulted() {
    assert_invalid_utf8_is_refused("as");
}

fn payloads() -> Vec<Vec<u8>> {
    let mut state = 0x1234_5678u32;
    let random = (0..131_195)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state as u8
        })
        .collect();
    vec![vec![], vec![255], vec![b'a'; 131_195], random]
}

#[test]
fn endpoints_round_trip_empty_single_byte_repetitive_and_incompressible_inputs() {
    for algorithm in ["gzip", "zlib"] {
        for payload in payloads() {
            for level in [0, 1, 6, 9] {
                let level = level.to_string();
                let compressed = source(
                    &format!("urn:compress:{algorithm}"),
                    &payload,
                    Some(("level", level.as_bytes())),
                )
                .unwrap();
                assert_eq!(
                    compressed.repr_type.media_type,
                    format!("application/{algorithm}")
                );
                let decoded = source(
                    &format!("urn:decompress:{algorithm}"),
                    &compressed.bytes,
                    Some(("as", b"application/x-audit")),
                )
                .unwrap();
                assert_eq!(decoded.bytes, payload, "{algorithm}, level {level}");
                assert_eq!(decoded.repr_type.media_type, "application/x-audit");
            }
        }
    }
}

struct PlannedRoundTrip {
    payload: Vec<u8>,
    media: &'static str,
    container: &'static str,
    streamed: bool,
}

#[async_trait::async_trait]
impl ikigai_core::Endpoint for PlannedRoundTrip {
    async fn invoke(&self, inv: &ikigai_core::Invocation<'_>) -> Result<Representation> {
        use ikigai_core::{Body, Bytes, ReprType};
        let body = if self.streamed {
            let chunks: Vec<Result<Bytes>> = self
                .payload
                .chunks(997)
                .map(|part| Ok(Bytes::copy_from_slice(part)))
                .collect();
            Body::once(futures::stream::iter(chunks))
        } else {
            Body::from(self.payload.clone())
        };
        let input = Representation::from_body(ReprType::new(self.media), body);
        let plan = inv.select_transreptor(self.media, self.container).unwrap();
        let compressed = inv.transrept(input, &plan).await?;
        assert_eq!(compressed.repr_type.media_type, self.container);
        let plan = inv.select_transreptor(self.container, self.media).unwrap();
        let output = inv.transrept(compressed, &plan).await?;
        output.collect(1024 * 1024).await
    }
}

#[test]
fn transreptors_round_trip_all_advertised_media_types_and_payload_shapes() {
    for container in [ikigai_compress::GZIP, ikigai_compress::ZLIB] {
        for &media in ikigai_compress::PAYLOAD_TYPES {
            for payload in payloads() {
                for streamed in [false, true] {
                    let endpoint = PlannedRoundTrip {
                        payload: payload.clone(),
                        media,
                        container,
                        streamed,
                    };
                    let kernel = Kernel::new(Arc::new(
                        ikigai_compress::space()
                            .bind(ikigai_core::Exact::new("urn:audit:round-trip"), endpoint),
                    ));
                    let request =
                        Request::new(Verb::Source, Iri::parse("urn:audit:round-trip").unwrap());
                    let output = block_on(kernel.issue(request, &Capability::root())).unwrap();
                    assert_eq!(
                        output.bytes, payload,
                        "{container}, {media}, streamed={streamed}"
                    );
                    assert_eq!(output.repr_type.media_type, media);
                }
            }
        }
    }
}
