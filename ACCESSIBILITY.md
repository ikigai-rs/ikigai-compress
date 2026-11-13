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
| Last reviewed | 2026-11-12 |

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

Source review covers the module-owned text in `src/lib.rs` and `src/incremental.rs`,
not wording supplied by the codec, core or a host:

- A refusal names the argument it is about; for `level`, `max-bytes` and `as` it quotes the value
  it refused.
- Output past `max-bytes` is refused, never truncated, and the refusal names the limit and how
  to raise it; a value over the ceiling says what to do instead.
- A level outside 0 to 9 is refused rather than clamped.
- Meaning is in plain text, without added color or terminal control sequences. Backticks
  and Rust debug escaping appear as literal characters; a host controls their presentation.

## How it is tested

Every named check has a result: Pass, Fail or Blocked, with evidence. A Pass covers only
its stated assertions, not overall accessibility. The checks below were run on 2026-11-12
against main commit `0bd8495489e95ab2e4dc5c8c141f24cc063c1462`, before this documentation
change. `cargo test` used Cargo 1.99.0 and rustc 1.99.0. CI repeats the Rust tests; it does
not repeat the manual source review. Tests check correctness and selected wording, not
whether someone can understand or use the output with assistive technology.

| Check | Result | Evidence |
|---|---|---|
| Refused level names its argument and range | Pass | `cargo test`: `src/lib.rs::tests::the_level_changes_the_bytes_and_is_bounded` checks `level` and `0..=9` for level 12. Does not test spoken comprehension of Rust range notation. |
| Decompression bound and ceiling refusals | Pass | `cargo test`: `src/lib.rs::tests::a_bomb_is_refused_naming_the_limit_not_truncated` checks `1024` and `refusing`; `max_bytes_is_itself_bounded_by_the_ceiling` checks the ceiling value. Does not evaluate readability of large byte counts. |
| Corrupt content and invalid media type name their arguments | Pass | `cargo test`: `src/lib.rs::tests::corrupt_input_is_an_error_about_content` checks `content` and `gzip`; `as_labels_the_payload_and_is_checked_for_shape` checks `as`. Does not check every codec error's wording or recovery advice. |
| Refusal through the engine | Pass | `cargo test`: `tests/pipeline.rs::the_bound_refuses_through_the_engine_too` checks the limit and `refusing`. Does not exercise a host's display. |
| Incremental limit messages | Pass | `cargo test`: `tests/incremental.rs::cumulative_bound_refuses_without_draining_the_source` checks `content` and `1024-byte limit`; `limits_span_multiple_output_chunks_and_members` checks `49152-byte limit` across gzip members and zlib chunks. Does not test assistive technology announcements. |
| Endpoint and argument descriptions exist | Pass | `cargo test`: `src/lib.rs::tests::every_endpoint_declares_a_typed_contract_and_no_capability` checks nonempty endpoint summaries for all four endpoints and nonempty input summaries; titles are covered only by source review. Does not assess clarity or a catalog's rendering. |
| Module-owned refusal text and descriptions: source review | Pass | Read `level`, `max_bytes`, `output_type`, `compress_description` and `decompress_description` in `src/lib.rs`, and `invalid` and `over_limit` in `src/incremental.rs` at the main commit above. Confirms argument names, quoted rejected option values, limit recovery guidance and no added color or terminal controls. This manual review is not a CI check and does not cover upstream wording or host rendering. |
| Automated accessibility checker | Blocked | No automated accessibility checker has been run on this output. This library provides no rendered host interface or configured browser checker. Rust tests are not an accessibility checker. |
| Browser rendering and manual keyboard | Blocked | No browser or person's keyboard pass has been run on this output; no host interface or interactive browser session was available for this review. |
| Screen reader | Blocked | No screen reader has been used on this output; no screen reader session was available for this review. |
| Screen magnifier | Blocked | No screen magnifier has been used on this output; no magnifier session was available for this review. |
| Speech input | Blocked | No speech-input software has been used on this output; no speech-input session was available for this review. |
| Switch access | Blocked | No switch-access software has been used on this output; no switch-access session was available for this review. |
| Braille display | Blocked | No braille display has been used on this output; no braille display was available for this review. |

## Known limitations

Found by reading the source on 2026-10-31 and confirmed in this review:

- **Limits are long numbers of bytes** (`1073741824`, `67108864`), with no `1 GiB` or `64 MiB`
  beside them, which is hard to take in by ear.
- **Programmer notation in a message**: the level refusal says `0..=9` (Rust's range syntax),
  and a refused value is quoted with Rust's debug escaping (`"\u{200b}"`), not as written.
- **A corrupt stream's reason is the codec's own wording** (`not a valid gzip stream: …`), which
  this repository does not control and which may not say what to do.
- Every message is English only.

## Contributing

Read this file before changing any output listed above: a refusal or an endpoint's description.
Name the argument, keep meaning in words, and say what to do. In the pull request's
accessibility section, give each check a row in a Check | Result | Evidence table with Pass,
Fail or Blocked, the test or command and tool version, or why it could not run. Keep unrun
checks as Blocked, including assistive technology nobody used. A historical run that CI does
not repeat is Blocked with its date, not Pass.

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
| 2026-11-12 | Record Pass / Fail / Blocked results, current test evidence and unrun assistive technology checks. |
