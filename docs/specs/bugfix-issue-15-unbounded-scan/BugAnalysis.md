# BugAnalysis — Issue #15 unbounded scan / MCP freeze

## summary

ChatGPT / MCP sessions die after extreme disk reads (hundreds of GB). Root cause is unbounded filesystem traversal and full-file streaming in tool layer, not a generic “agent is dumb” problem.

## tbp.phenomenon

| | |
|---|---|
| Ideal | `list_files` / `search_text` / `list_dir` / `read_file` stay bounded: prune ignored dirs, cap visited entries, stop reading once output is capped |
| Actual | `list_files` / `search_text` WalkDir into `node_modules` / `target` / `.git` (continue ≠ prune); outside-workspace absolute reads skip ignore rules entirely; `read_file_streaming` still scans whole files for `total_lines` after content is capped |
| Gap | Disk IO and latency unbounded → MCP worker blocks → session terminated |

## tbp.boundary

- Layer: `code` (`src-tauri/src/tools/file.rs`, `workspace.rs`)
- Related: harness baseline prune already fixed in PR #14 (`harness/state.rs` `filter_entry`); **tool walks were not updated**
- Not primarily: FRP / tunnel / UI

## tbp.timeline

1. Agent calls `list_files` / `search_text` with broad path (workspace root or absolute external path), or `read_file` on a multi-GB file
2. WalkDir visits millions of entries (no prune / no visit budget) **or** streaming read keeps reading after `max_bytes`
3. Disk saturates; MCP RPC worker blocks; ChatGPT times out / session terminated

## testPlan (SRC-3)

1. Unit: WalkDir over a tree with huge ignored subtree must not visit files inside ignored dirs (prune via `filter_entry`)
2. Unit: visit budget stops walk with `truncated=true` before matching `max_results` if needed
3. Unit: `read_file_streaming` on a file larger than `max_bytes` must not read the whole file once selection is complete
4. `cargo test --manifest-path src-tauri/Cargo.toml` green; external-read security tests still pass for small fixtures

## rootCauseAnalysis

```json
{
  "mode": "complex",
  "attributionLayer": "code",
  "hypotheses": [
    {
      "id": "H1",
      "statement": "WalkDir in list_files/search_text does not filter_entry-prune ignored directories",
      "attributionLayer": "code",
      "status": "confirmed",
      "evidence": ["file.rs continues on ignored dirs; WalkDir still descends", "harness/state.rs already documents and uses filter_entry for the same class of bug"],
      "counterEvidence": []
    },
    {
      "id": "H2",
      "statement": "is_ignored_path returns false for any path outside workspace root, so absolute external list/search has zero ignore filtering",
      "attributionLayer": "code",
      "status": "confirmed",
      "evidence": ["workspace.rs strip_prefix fail → return false", "call_tool_security allows absolute external list/search by design"],
      "counterEvidence": []
    },
    {
      "id": "H3",
      "statement": "read_file_streaming reads entire file to count total_lines after content already capped",
      "attributionLayer": "code",
      "status": "confirmed",
      "evidence": ["streaming loop only stops at EOF", "PR #14 fixed memory not disk scan"],
      "counterEvidence": []
    },
    {
      "id": "H4",
      "statement": "Path escape on writes (patch to wrong path) is the same bug as scan",
      "attributionLayer": "code",
      "status": "ruled_out",
      "evidence": [],
      "counterEvidence": ["apply_patch rejects absolute/traversal in security tests; separate from scan IO"]
    },
    {
      "id": "H5",
      "statement": "Only agent_behavior with no server bug",
      "attributionLayer": "agent_behavior",
      "status": "ruled_out",
      "evidence": [],
      "counterEvidence": ["Even a reasonable recursive search of a normal workspace with node_modules/target is unbounded without prune"]
    }
  ],
  "forkPoint": "Success: small trees / no ignored bulk / small files. Failure: WalkDir enters ignored or external bulk trees, or multi-GB read_file keeps scanning after cap.",
  "whyChain": [
    {
      "level": 1,
      "observation": "Hundreds of GB disk read and MCP freeze",
      "why": "Why so much disk IO from one tool call?",
      "because": "Filesystem walk or file stream does not stop early"
    },
    {
      "level": 2,
      "observation": "list_files/search_text use WalkDir with continue-on-ignored",
      "why": "Why does ignored filtering not limit IO?",
      "because": "WalkDir requires filter_entry to prune; continue still descends"
    },
    {
      "level": 3,
      "observation": "External absolute reads are allowed and ignore rules do not apply outside root",
      "why": "Why can an agent amplify this further?",
      "because": "resolve_read_path permits explicit external paths; is_ignored_path short-circuits to false"
    },
    {
      "level": 4,
      "observation": "read_file_streaming still reads to EOF after max_bytes",
      "why": "Why can a single read also burn hundreds of GB?",
      "because": "total_lines accounting keeps scanning after selection is complete"
    }
  ],
  "primaryCause": "Tool-layer WalkDir lacks ignored-directory prune and visit budgets; read_file keeps scanning after content cap",
  "contributingFactors": [
    "Absolute external read allowance without external ignore/visit caps",
    "Agent broad **/* searches on large workspaces"
  ],
  "rootCauseStatement": "WalkDir without filter_entry prune (plus external paths skipping ignore rules) and full-file streaming after max_bytes, under large trees or multi-GB files, causes unbounded disk IO that freezes MCP and terminates the session",
  "confidence": "high",
  "evidenceGaps": [
    "No user-provided tool-call log naming the exact tool/path for the original report"
  ]
}
```

## fixPlan

1. `list_files` / `search_text`: `WalkDir.filter_entry` prune using `is_ignored_path` (and component-name excludes for external paths)
2. Add hard `max_visited` budget independent of match count; set `truncated` + warning when hit
3. `read_file_streaming`: stop reading once selection is complete (`overflow` or past `end_line`); report lines seen / truncated honestly
4. Regression tests for prune + early stop + visit budget
5. Keep intentional small-fixture external read tests green

## affectedFiles (planned)

- `src-tauri/src/tools/file.rs`
- `src-tauri/src/tools/workspace.rs` (optional helper for external basename excludes)
- tests under `src-tauri` / `#[cfg(test)]` in `file.rs`
