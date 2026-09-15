# agent-response

A small Rust command wrapper that returns predictable, compact responses to agents.

## Build and use

```sh
cargo build --release
./target/release/agent-response -- cargo check
./target/release/agent-response --pattern 'error|warning|panic' -- cargo test
./target/release/agent-response --literal --pattern 'ERROR:' -- your-command
./target/release/agent-response --no-stdout -- your-command
./target/release/agent-response --failure-only -- your-command
./target/release/agent-response --count -- your-command
```

Pass the executable and its arguments after `--`. The wrapper does not interpret
shell syntax; explicitly invoke a shell if needed.

## Response contract

- On success, stdout is `ok\n`, or the selected stdout **line count** with `--count`.
- On failure, stdout is empty. The child exit code is preserved. On Unix, signal
  termination maps to `128 + signal`. Wrapper errors use 125 (which a child may
  also return; wrapper diagnostics identify their source).
- Stderr is retained, followed by stdout lines matching the case-insensitive
  byte regex `err|arning`, under a `[stdout diagnostics]` heading. This pattern
  deliberately matches substrings, including `berry`; it is not semantic error
  detection. Matching a diagnostic does not change a successful child status.
- `--pattern PATTERN` changes the selector. `--literal` treats it as literal text;
  `--regex` selects regex interpretation (the default). Last mode flag wins.
  Matching is ASCII case-insensitive; regex inline flags can override this.
- `--no-stdout` disables stdout selection. `--failure-only` suppresses all
  diagnostics when the child succeeds. Default behavior shows diagnostics on
  success as well as failure.
- Count means selected stdout lines, not occurrences, files, or a tool's own
  count. A separate search adapter would be needed for search-specific counts.
- Regexes are validated before launching the child. Invalid regex, spawn,
  capture, or output errors produce wrapper failures.

## Boundaries

Both streams are drained concurrently to avoid pipe-capacity deadlock. Each
retains at most 1 MiB; stdout also has a 1 MiB per-line buffer. Truncation is
reported. For an oversized line, only its retained prefix is searched, so later
matches can be omitted; `--count` refuses to report an exact result after any
stdout truncation. Output is grouped stderr-first, not chronological or deduplicated.
No stack-trace continuation lines are inferred. Arbitrary output bytes are
preserved, including terminal escape sequences.

This version waits for command completion and pipe closure. It has no timeout or
process-tree cancellation yet; a hung child or descendant holding a pipe open
can keep it waiting. Stdin is inherited. Use it for trusted, bounded commands.

## Verification

```sh
cargo test --locked
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
```

Tests include explicit matching contracts, binary and unterminated lines,
truncation, reader failures, 10,000 reproducible randomized byte/chunk cases,
invalid regex without child execution, missing executables, exit/signal status,
and simultaneous large streams. Process integration tests currently target Unix.
These checks provide evidence, not a guarantee against every possible failure.

## MCP integration (planned)

A thin stdio MCP server can expose the same runner with a discoverable tool
schema: command, argument array, selector, regex/literal mode, response mode,
and diagnostic policy. It should share the CLI implementation and contract.
MCP clients must explicitly configure the server; installing the CLI alone does
not make it discoverable. Before exposing arbitrary command execution through
MCP, add deadlines/cancellation and define the execution permissions clearly.
MCP transport is not implemented in this version.
