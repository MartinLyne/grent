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

Both `agent-response` and `ar` are built from the same implementation:

```sh
./target/release/ar -- your-command
./target/release/ar --filter 'warn|err' -- your-command
./target/release/ar --filter '*' -- your-command
```

`ar` also names the standard archive utility. Use the explicit path or an
isolated agent environment to avoid shadowing it in build tools. To install
only the long name, use `cargo install --path . --bin agent-response`.

Pass the executable and its arguments after `--`. The wrapper does not interpret
shell syntax; explicitly invoke a shell if needed.

## Response contract

| Invocation | Successful command | Failed command |
| --- | --- | --- |
| Default | Only `ok` on stdout | Smart-merge diagnostics on stderr |
| `--filter PATTERN` (alias `--pattern`) | Matching lines from both streams, on their original streams; no `ok` | All stderr plus matching stdout on stderr |
| `--filter '*'` | All output passed through unchanged | All output passed through unchanged |
| `--count` | Selected stdout line count only | Smart-merge diagnostics on stderr |

- Default failure selection is the case-insensitive byte regex `err|arning`.
  Stderr is retained, followed by selected stdout under `[stdout diagnostics]`.
  This deliberately matches substrings, including `berry`; it is not semantic
  error detection. Matching a diagnostic does not change the child exit status.
- Explicit filters select successful logs from **both** stdout and stderr.
  No matches produces empty output with exit 0. Default success suppresses all
  child output, including warnings on stderr.
- `--literal` treats the pattern as literal text; `--regex` selects regex
  interpretation (the default). Last mode flag wins. Matching is ASCII
  case-insensitive; regex inline flags can override this.
- A standalone `*` in regex mode is special: it means all output, not a regex
  quantifier. Quote it to prevent shell expansion. `--literal --filter '*'`
  searches for actual asterisks.
- `--no-stdout` disables stdout selection. `--failure-only` overrides filtered
  success output, restoring `ok`. `--count` takes precedence over log output.
- Count means selected stdout lines, not occurrences, files, or a tool's own
  count. With `--filter '*' --count`, all stdout lines are counted. A separate
  search adapter would be needed for search-specific counts.
- Child exit codes are preserved. On Unix, signal termination maps to
  `128 + signal`. Wrapper errors use 125 (which a child may also return;
  wrapper diagnostics identify their source).
- Regexes are validated before launching the child. Invalid regex, spawn,
  capture, or output errors produce wrapper failures.

```sh
agent-response -- your-command                    # success: ok
agent-response --filter 'warn|err' -- your-command # success: matching logs
agent-response --filter '*' -- your-command        # normal command output
```

## Boundaries

Unmodified `--filter '*'` inherits the command's output streams directly, with
no capture limit, added headers, or completion buffering. Combining it with
`--count`, `--failure-only`, or `--no-stdout` uses capture instead.

In capture modes, both streams are drained concurrently to avoid pipe-capacity deadlock. Each
retains at most 1 MiB; stdout also has a 1 MiB per-line buffer. Truncation is
reported when logs are shown; ordinary success still returns only `ok`. For an oversized line, only its retained prefix is searched, so later
matches can be omitted; `--count` refuses to report an exact result after any
stdout truncation. Failure output is grouped stderr-first, not chronological or deduplicated.
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
