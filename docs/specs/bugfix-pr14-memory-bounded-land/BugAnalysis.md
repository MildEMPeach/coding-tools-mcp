# BugAnalysis — PR #14 land fixes

## Phenomenon

PR #14 streaming `read_file` claimed identical semantics to the in-memory path, but incomplete UTF-8 at EOF returned `Ok` (empty/partial content, phantom lines) instead of `UNSUPPORTED_ENCODING`. `capture_baseline` skip list did not prune `WalkDir`, so home-dir workspaces still descended into OneDrive/`node_modules`.

## Root cause

1. `utf8_carry` after the read loop was never checked; trailing-line logic treated leftover incomplete bytes as a normal line end.
2. `should_skip` only gated hashing; walker still enumerated skipped trees.

## Fix plan

1. Reject non-empty `utf8_carry` before trailing-line flush.
2. `WalkDir::filter_entry` prune when `should_skip`; case-insensitive OS/cloud directory names.
3. Regression tests for EOF UTF-8 and pruned baseline capture.

## Test plan

- `incomplete_utf8_at_eof_errors`
- `incomplete_utf8_after_valid_line_errors`
- `utf8_straddling_chunk_boundary_ok`
- `streaming_matches_legacy_semantics`
- `capture_baseline_prunes_skipped_directories`
