//! Lazy adapters around flate2's incremental codecs. The gzip writers own all
//! framing and checksum handling; the bounded sink supplies backpressure.
use std::io::{self, Write};

use flate2::write::{GzEncoder, MultiGzDecoder, ZlibEncoder};
use flate2::{Compression, Decompress, FlushDecompress, GzBuilder, Status};
use futures_util::{stream, StreamExt};
use ikigai_core::{Body, ByteStream, Bytes, Error, Result};

const CHUNK: usize = 16 * 1024;

struct Input {
    stream: ByteStream,
    chunk: Bytes,
    eof: bool,
}

impl Input {
    fn new(body: Body) -> Self {
        Self {
            stream: body.into_stream(),
            chunk: Bytes::new(),
            eof: false,
        }
    }

    async fn fill(&mut self) -> Result<()> {
        while self.chunk.is_empty() && !self.eof {
            match self.stream.next().await {
                Some(chunk) => self.chunk = chunk?,
                None => self.eof = true,
            }
        }
        Ok(())
    }

    fn consume(&mut self, count: usize) {
        self.chunk = self.chunk.slice(count..);
    }
}

fn invalid(gzip: bool, detail: impl std::fmt::Display) -> Error {
    let algorithm = if gzip { "gzip" } else { "zlib" };
    Error::InvalidArgument {
        name: "content".into(),
        detail: format!("not a valid {algorithm} stream: {detail}"),
    }
}

fn over_limit(limit: usize) -> Error {
    Error::InvalidArgument {
        name: "content".into(),
        detail: format!(
            "decompressed output exceeds the {limit}-byte limit set by `max-bytes`; refusing rather than returning a truncated representation (raise `max-bytes`, up to {})",
            super::MAX_BYTES_CEILING,
        ),
    }
}

/// A single output chunk. WouldBlock pauses flate2 without flushing the codec
/// or inserting sync markers. The cumulative bound survives every drain and
/// every gzip member. No bytes beyond the limit are retained.
#[derive(Debug)]
struct Output {
    bytes: Vec<u8>,
    remaining: Option<usize>,
    exceeded: bool,
}

impl Output {
    fn new(limit: Option<usize>) -> Self {
        Self {
            bytes: Vec::new(),
            remaining: limit,
            exceeded: false,
        }
    }

    fn take(&mut self) -> Bytes {
        std::mem::take(&mut self.bytes).into()
    }
}

impl Write for Output {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        if self.remaining == Some(0) {
            self.exceeded = true;
            return Err(io::Error::other("decompression limit exceeded"));
        }
        let count = data
            .len()
            .min(CHUNK - self.bytes.len())
            .min(self.remaining.unwrap_or(usize::MAX));
        if count == 0 {
            return Err(io::ErrorKind::WouldBlock.into());
        }
        self.bytes.extend_from_slice(&data[..count]);
        if let Some(remaining) = &mut self.remaining {
            *remaining -= count;
        }
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

enum Writer {
    GzipEncoder(GzEncoder<Output>),
    ZlibEncoder(ZlibEncoder<Output>),
    GzipDecoder(MultiGzDecoder<Output>),
}

impl Writer {
    fn output(&mut self) -> &mut Output {
        match self {
            Self::GzipEncoder(codec) => codec.get_mut(),
            Self::ZlibEncoder(codec) => codec.get_mut(),
            Self::GzipDecoder(codec) => codec.get_mut(),
        }
    }

    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        match self {
            Self::GzipEncoder(codec) => codec.write(data),
            Self::ZlibEncoder(codec) => codec.write(data),
            Self::GzipDecoder(codec) if data.is_empty() => codec.flush().map(|()| 0),
            Self::GzipDecoder(codec) => codec.write(data),
        }
    }

    fn finish(&mut self) -> io::Result<()> {
        match self {
            Self::GzipEncoder(codec) => codec.try_finish(),
            Self::ZlibEncoder(codec) => codec.try_finish(),
            Self::GzipDecoder(codec) => codec.try_finish(),
        }
    }
}

pub(super) fn compress(body: Body, gzip: bool, level: u32) -> Body {
    let output = Output::new(None);
    let codec = if gzip {
        Writer::GzipEncoder(
            GzBuilder::new()
                .mtime(super::MTIME_NONE)
                .operating_system(super::OS_UNKNOWN)
                .write(output, Compression::new(level)),
        )
    } else {
        Writer::ZlibEncoder(ZlibEncoder::new(output, Compression::new(level)))
    };
    transform(body, codec, None)
}

fn transform(body: Body, codec: Writer, limit: Option<usize>) -> Body {
    let state = (Input::new(body), codec, false);
    Body::once(stream::try_unfold(
        state,
        move |(mut input, mut codec, mut done)| async move {
            loop {
                if done {
                    return Ok(None);
                }
                // Drain pending codec output before fetching another upstream
                // chunk. Encoder writes never insert sync-flush markers; the
                // decoder may flush already decoded bytes without ending a member.
                let result = if input.eof {
                    codec.finish().map(|()| {
                        done = true;
                        0
                    })
                } else {
                    codec.write(&input.chunk)
                };
                let consumed = match result {
                    Ok(count) => count,
                    Err(_) if codec.output().exceeded => return Err(over_limit(limit.unwrap())),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => 0,
                    Err(error) => {
                        return Err(if limit.is_some() {
                            invalid(true, error)
                        } else {
                            Error::Endpoint(format!("compression: {error}"))
                        })
                    }
                };
                input.consume(consumed);
                let out = codec.output().take();
                if !out.is_empty() {
                    return Ok(Some((out, (input, codec, done))));
                }
                if done {
                    return Ok(None);
                }
                if input.chunk.is_empty() {
                    input.fill().await?;
                } else if consumed == 0 {
                    return Err(invalid(limit.is_some(), "codec made no progress"));
                }
            }
        },
    ))
}

pub(super) fn decompress(body: Body, gzip: bool, limit: usize) -> Body {
    if gzip {
        return transform(
            body,
            Writer::GzipDecoder(MultiGzDecoder::new(Output::new(Some(limit)))),
            Some(limit),
        );
    }
    // The raw incremental API exposes StreamEnd, which is needed to reject
    // truncated zlib streams (write::ZlibDecoder::try_finish does not check it).
    let state = (Input::new(body), Decompress::new(true), 0usize, false);
    Body::once(stream::try_unfold(
        state,
        move |(mut input, mut codec, mut emitted, done)| async move {
            if done {
                return Ok(None);
            }
            loop {
                let mut out = vec![0; CHUNK.min(limit - emitted + 1)];
                let before_in = codec.total_in();
                let before_out = codec.total_out();
                let status = codec
                    .decompress(&input.chunk, &mut out, FlushDecompress::None)
                    .map_err(|error| invalid(false, error))?;
                let consumed = (codec.total_in() - before_in) as usize;
                let produced = (codec.total_out() - before_out) as usize;
                input.consume(consumed);
                if produced > limit - emitted {
                    return Err(over_limit(limit));
                }
                emitted += produced;
                out.truncate(produced);
                let done = status == Status::StreamEnd;
                if !out.is_empty() {
                    return Ok(Some((out.into(), (input, codec, emitted, done))));
                }
                if done {
                    return Ok(None);
                }
                if consumed == 0 {
                    if input.eof {
                        return Err(invalid(false, "unexpected end of stream"));
                    }
                    if !input.chunk.is_empty() {
                        return Err(invalid(false, "decoder made no progress"));
                    }
                    input.fill().await?;
                }
            }
        },
    ))
}
