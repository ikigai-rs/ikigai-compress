//! Bounded-output flate2 state machines. Gzip framing is handled here because
//! flate2's pure Rust `Compress`/`Decompress` support raw deflate and zlib only.
use flate2::{Compress, Compression, Crc, Decompress, FlushCompress, FlushDecompress, Status};
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

    // Only pull upstream when the current chunk has been consumed. Empty chunks
    // are legal and do not signify EOF.
    async fn fill(&mut self) -> Result<bool> {
        while self.chunk.is_empty() && !self.eof {
            match self.stream.next().await {
                Some(chunk) => self.chunk = chunk?,
                None => self.eof = true,
            }
        }
        Ok(!self.chunk.is_empty())
    }

    async fn byte(&mut self) -> Result<u8> {
        if !self.fill().await? {
            return Err(invalid(true, "unexpected end of stream"));
        }
        let byte = self.chunk[0];
        self.consume(1);
        Ok(byte)
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

struct Encoder {
    input: Input,
    codec: Compress,
    gzip: bool,
    header: Option<Bytes>,
    trailer: Option<Bytes>,
    crc: Crc,
    done: bool,
}

pub(super) fn compress(body: Body, gzip: bool, level: u32) -> Body {
    let header = gzip.then(|| {
        // Match GzBuilder's deterministic header, including its level hint.
        let xfl = match level {
            9 => 2,
            0..=1 => 4,
            _ => 0,
        };
        let mut header = vec![0x1f, 0x8b, 8, 0];
        header.extend_from_slice(&super::MTIME_NONE.to_le_bytes());
        header.extend_from_slice(&[xfl, super::OS_UNKNOWN]);
        Bytes::from(header)
    });
    let state = Encoder {
        input: Input::new(body),
        codec: Compress::new(Compression::new(level), !gzip),
        gzip,
        header,
        trailer: None,
        crc: Crc::new(),
        done: false,
    };
    Body::once(stream::try_unfold(state, |mut state| async move {
        if let Some(header) = state.header.take() {
            return Ok(Some((header, state)));
        }
        if state.done {
            return Ok(state.trailer.take().map(|trailer| (trailer, state)));
        }
        loop {
            let mut out = vec![0; CHUNK];
            let before_in = state.codec.total_in();
            let before_out = state.codec.total_out();
            let flush = if state.input.eof {
                FlushCompress::Finish
            } else {
                FlushCompress::None
            };
            let status = state
                .codec
                .compress(&state.input.chunk, &mut out, flush)
                .map_err(|e| Error::Endpoint(format!("compression: {e}")))?;
            let consumed = (state.codec.total_in() - before_in) as usize;
            let produced = (state.codec.total_out() - before_out) as usize;
            state.crc.update(&state.input.chunk[..consumed]);
            state.input.consume(consumed);
            out.truncate(produced);
            if status == Status::StreamEnd {
                state.done = true;
                if state.gzip {
                    let mut trailer = Vec::with_capacity(8);
                    trailer.extend_from_slice(&state.crc.sum().to_le_bytes());
                    trailer.extend_from_slice(&state.crc.amount().to_le_bytes());
                    state.trailer = Some(trailer.into());
                }
            }
            if !out.is_empty() {
                return Ok(Some((out.into(), state)));
            }
            if state.done {
                return Ok(state.trailer.take().map(|trailer| (trailer, state)));
            }
            if consumed == 0 {
                if state.input.eof || !state.input.chunk.is_empty() {
                    return Err(Error::Endpoint("compression made no progress".into()));
                }
                state.input.fill().await?;
            }
        }
    }))
}

#[derive(Clone, Copy)]
enum Phase {
    Header,
    Deflate,
    Trailer,
    BetweenMembers,
    Done,
}

struct Decoder {
    input: Input,
    codec: Decompress,
    gzip: bool,
    phase: Phase,
    crc: Crc,
    limit: usize,
    emitted: usize,
}

pub(super) fn decompress(body: Body, gzip: bool, limit: usize) -> Body {
    let state = Decoder {
        input: Input::new(body),
        codec: Decompress::new(!gzip),
        gzip,
        phase: if gzip { Phase::Header } else { Phase::Deflate },
        crc: Crc::new(),
        limit,
        emitted: 0,
    };
    Body::once(stream::try_unfold(state, |mut state| async move {
        loop {
            match state.phase {
                Phase::Header => {
                    gzip_header(&mut state.input).await?;
                    state.phase = Phase::Deflate;
                }
                Phase::Deflate => {
                    // Probe at most one byte beyond the cumulative bound, even
                    // when one compressed chunk expands to gigabytes.
                    let capacity = CHUNK.min(state.limit - state.emitted + 1);
                    let mut out = vec![0; capacity];
                    let before_in = state.codec.total_in();
                    let before_out = state.codec.total_out();
                    let status = state
                        .codec
                        .decompress(&state.input.chunk, &mut out, FlushDecompress::None)
                        .map_err(|e| invalid(state.gzip, e))?;
                    let consumed = (state.codec.total_in() - before_in) as usize;
                    let produced = (state.codec.total_out() - before_out) as usize;
                    state.input.consume(consumed);
                    out.truncate(produced);
                    if produced > state.limit - state.emitted {
                        return Err(Error::InvalidArgument {
                            name: "content".into(),
                            detail: format!(
                                "decompressed output exceeds the {}-byte limit set by `max-bytes`; refusing rather than returning a truncated representation (raise `max-bytes`, up to {})",
                                state.limit, super::MAX_BYTES_CEILING,
                            ),
                        });
                    }
                    state.emitted += produced;
                    state.crc.update(&out);
                    if status == Status::StreamEnd {
                        state.phase = if state.gzip {
                            Phase::Trailer
                        } else {
                            Phase::Done
                        };
                    }
                    if !out.is_empty() {
                        return Ok(Some((out.into(), state)));
                    }
                    if status != Status::StreamEnd && consumed == 0 {
                        if !state.input.chunk.is_empty() {
                            return Err(invalid(state.gzip, "decoder made no progress"));
                        }
                        if !state.input.fill().await? {
                            return Err(invalid(state.gzip, "unexpected end of stream"));
                        }
                    }
                }
                Phase::Trailer => {
                    let mut trailer = [0; 8];
                    for byte in &mut trailer {
                        *byte = state.input.byte().await?;
                    }
                    if trailer[..4] != state.crc.sum().to_le_bytes()
                        || trailer[4..] != state.crc.amount().to_le_bytes()
                    {
                        return Err(invalid(true, "CRC or uncompressed size mismatch"));
                    }
                    state.phase = Phase::BetweenMembers;
                }
                Phase::BetweenMembers => {
                    if !state.input.fill().await? {
                        return Ok(None);
                    }
                    state.codec.reset(false);
                    state.crc.reset();
                    state.phase = Phase::Header;
                }
                // Match the existing zlib endpoint: one stream, ignoring bytes
                // after its checksum (gzip instead validates every member).
                Phase::Done => return Ok(None),
            }
        }
    }))
}

// Parse optional gzip fields without retaining names, comments or extra data.
// Header CRC covers every byte preceding FHCRC, including zero terminators.
async fn gzip_header(input: &mut Input) -> Result<()> {
    let mut header = [0; 10];
    for byte in &mut header {
        *byte = input.byte().await?;
    }
    if header[..3] != [0x1f, 0x8b, 8] || header[3] & 0xe0 != 0 {
        return Err(invalid(true, "invalid header"));
    }
    let flags = header[3];
    let mut crc = Crc::new();
    crc.update(&header);
    if flags & 4 != 0 {
        let lo = header_byte(input, &mut crc).await?;
        let hi = header_byte(input, &mut crc).await?;
        for _ in 0..u16::from_le_bytes([lo, hi]) {
            header_byte(input, &mut crc).await?;
        }
    }
    for flag in [8, 16] {
        if flags & flag != 0 {
            while header_byte(input, &mut crc).await? != 0 {}
        }
    }
    if flags & 2 != 0 {
        let actual = u16::from_le_bytes([input.byte().await?, input.byte().await?]);
        if actual != crc.sum() as u16 {
            return Err(invalid(true, "header CRC mismatch"));
        }
    }
    Ok(())
}

async fn header_byte(input: &mut Input, crc: &mut Crc) -> Result<u8> {
    let byte = input.byte().await?;
    crc.update(&[byte]);
    Ok(byte)
}
