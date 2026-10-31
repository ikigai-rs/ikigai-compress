# Accessibility

What `ikigai-compress` emits that people read, the rules that output follows, how it is tested,
and how to report a barrier. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only (no HTML, SVG or color) |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-compress/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-compress/security/advisories/new) |
| Last reviewed | 2026-10-31 |

## What it emits that people read

This is a library: four endpoints (`urn:compress:gzip`, `urn:compress:zlib`,
`urn:decompress:gzip`, `urn:decompress:zlib`) a host binds. Their answers are bytes, compressed
or as the container held them, and are not meant to be read here. What people read is:

- **Refusals** (`src/lib.rs`, `src/incremental.rs`): an `InvalidArgument` naming the argument
  with a sentence: `level` (``expected an integer 0..=9, got "12"``), `max-bytes` (not a number,
  or over the ceiling: ``… exceeds this module's ceiling of 1073741824 bytes; decompress in
  pieces rather than asking the host for more``), `as` (``expected a media type like
  `text/turtle`, got …``), and `content` (`not a valid gzip stream: …`, or output past the
  limit: ``decompressed output exceeds the 1024-byte limit set by `max-bytes`; refusing rather
  than returning a truncated representation (raise `max-bytes`, up to …)``). A missing `content`
  is core's `MissingArgument`.
- **Self-description**: each endpoint's title and summary, and each argument's summary
  (`compress_description`, `decompress_description`), which a catalog, an MCP tool list or a
  person reading Meta sees.

Not this repository's: how a host shows any of this, and the display of core's error kinds.

## The rules that output follows

Verified by reading the source:

- A refusal names the argument it is about; for `level`, `max-bytes` and `as` it quotes the value
  it refused.
- Output past `max-bytes` is refused, never truncated, and the refusal names the limit and how
  to raise it; a value over the ceiling says what to do instead.
- A level outside 0 to 9 is refused rather than clamped.
- No color, no terminal control sequences and no markup are written.

## How it is tested

- Unit tests in `src/lib.rs`: `the_level_changes_the_bytes_and_is_bounded` (the refusal says
  `0..=9`), `a_bomb_is_refused_naming_the_limit_not_truncated`,
  `max_bytes_is_itself_bounded_by_the_ceiling`, `corrupt_input_is_an_error_about_content` and
  `as_labels_the_payload_and_is_checked_for_shape` (a refusal names its argument, or the
  ceiling).
- `tests/pipeline.rs`: `the_bound_refuses_through_the_engine_too` (names the limit and says
  `refusing`); `tests/incremental.rs`: `cumulative_bound_refuses_without_draining_the_source`
  and `limits_span_multiple_output_chunks_and_members` (the message names the limit in bytes).

These test wording for correctness; none was written as an accessibility test.
**No automated accessibility checker, browser or assistive technology has been used on this
output.**

## Known limitations

Found by reading the source on 2026-10-31:

- **Limits are long numbers of bytes** (`1073741824`, `67108864`), with no `1 GiB` or `64 MiB`
  beside them, which is hard to take in by ear.
- **Programmer notation in a message**: the level refusal says `0..=9` (Rust's range syntax),
  and a refused value is quoted with Rust's debug escaping (`"\u{200b}"`), not as written.
- **A corrupt stream's reason is the codec's own wording** (`not a valid gzip stream: …`), which
  this repository does not control and which may not say what to do.
- Every message is English only.

## Contributing

Read this file before changing any output listed above: a refusal or an endpoint's description.
Name the argument, keep meaning in words, and say what to do. Say in the pull request what was
tested, with what, and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-compress/issues/new?labels=accessibility)
with the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and the host and any assistive
technology you used, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-compress/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
