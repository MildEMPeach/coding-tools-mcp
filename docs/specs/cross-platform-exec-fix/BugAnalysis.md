# Cross-platform execution contract repair

## Gap and boundary

A valid workspace absolute cwd is rejected. A venv Python symlink resolving to
`python3.13` is rejected as an unsupported `.13` script; canonicalizing the entry
also changes Python's environment identity. Mutation detection treats the absolute
executable itself as an external write target. POSIX shell splitting consumes
Windows path backslashes. Spawn failures report `ok: true` and receive a completed
operation hint. Reproduced on the installed 0.2.1 server; the same implementation
is present in upstream 8886513.

## Root cause worksheet

1. Observations: relative writes and apply_patch succeed; absolute cwd and the
   fixed interpreter fail before process launch. Local invocation of that
   interpreter succeeds. Transport authentication succeeds in all these cases.
2. Competing hypotheses: read-only credentials and unavailable Python do not
   explain successful writes and direct interpreter execution; policy and
   executable resolution do explain the selective failures.
3. Causal boundary: policy -> shared dispatcher -> exec worker -> MCP envelope.
   The Windows parser problem is independent of authentication and transport.
4. Cause: syntactic path rejection replaces workspace containment; executable
   canonicalization is used both for security checks and launch identity; status
   metadata equates tool transport success with command completion.
5. Counterfactual: equivalent relative/absolute cwd and relative writes via the
   same interpreter must succeed; genuine workspace escapes and launch failures
   must remain rejected. Tests below distinguish this from merely hiding errors.

## Repair and acceptance

- Use one platform-aware command parser in policy and execution; preserve Windows
  backslashes and double-quote rules, with native process launch (no shell).
- Resolve relative/absolute execution cwd, requiring canonical workspace containment.
- Exclude the executable from mutation target scanning; check literal absolute
  targets against canonical workspace containment, including symlink ancestors.
- Validate the executable target but launch its original entry to preserve venv
  identity. Allow versioned Python names, and explicit allowlisted PATH entries.
- Report launch failures as tool errors. Keep nonzero exit's established
  transport/command distinction, without a misleading completed-success hint.
- Test successful writes, cwd equivalence, venv prefix, Windows quoting, missing
  executable, running/nonzero status, traversal, external paths and symlink escapes.
- Run Rust unit/integration tests, frontend checks and diff checks. Build committed
  0.2.2 candidates, then validate real MCP writes and error responses on both OSes.

This retains the existing `policy_only` execution boundary; it does not introduce
an OS sandbox or enable workspace-external writes.
