# grent

Explicit command results and locally retained logs for coding agents. Written in Rust; supports macOS and Linux. `agent-response` is the same executable under a descriptive name.

Use grent when an agent needs a success signal, an exact line count, or a small preview followed by queries against the original output. An ordinary shell pipeline already handles many of these reductions well.

## Install

```sh
cargo install --git https://github.com/MartinLyne/grent --locked
# Or from a checkout:
cargo install --path . --locked
```

Both executables are installed. Rust 1.79 or later is required. This project is not published on crates.io.

## Choose the answer you need

```sh
grent check -- cargo check                       # ok on success
grent count --filter 'warning' -- cargo check     # matching stdout lines
grent exists --literal --filter 'TODO' -- cat src/main.rs
grent preview --lines 10 --max-bytes 2048 -- your-command
```

Pass an executable and its arguments after `--`. To run a pipeline, explicitly invoke a shell; use its `pipefail` option when every stage's status matters.

| Mode | Successful result | Default selection |
| --- | --- | --- |
| `check` (also the default) | `ok` | Suppresses successful logs |
| `count` | Integer matching-line count | All stdout lines |
| `exists` | `true` or `false` | All stdout lines |
| `preview` | Up to 20 matching lines / 8 KiB | Stdout |

`exists` returning `false` is a successful answer, with exit status 0. Count means lines, not occurrences or files. Use `--stream stderr` to query stderr. A final line without a newline still counts.

Filters default to ASCII case-insensitive byte regexes. `--literal` selects literal text; `--regex` switches back. A standalone `--filter '*'` in regex mode selects everything; quote the star. In literal mode it matches actual asterisks. Regexes are validated before the child runs.

## Query the original output

Use `--json` to receive a run ID, then query it without executing the original command again:

```sh
result=$(grent preview --json --lines 10 -- your-command)
run_id=$(printf '%s' "$result" | jq -r .run_id)
grent read "$run_id" --mode count --filter 'warning'
grent read "$run_id" --mode preview --offset 10 --lines 10
grent read "$run_id" --stream stderr --mode full --max-bytes 65536
grent forget "$run_id"
```

`read` defaults to `preview`. Its modes are `count`, `exists`, `preview`, and `full`. `full` removes the line limit but retains `--max-bytes`. Offsets skip matching lines. Counts always cover the entire selected stream; pagination applies to displayed output.

Run/read JSON includes `exit_code`, `source_exit_code`, `run_id`, `mode`, `incomplete`, and `truncated`, plus the requested answer. It includes only the requested output, rather than embedding full logs for the model to ignore. A successful read returns exit status 0 even if the original command failed; `source_exit_code` records the original result. Wrapper errors use an `error` field. Help and version remain plain text.

## Failures and smart-merge

A failed command preserves its exit status. Grent returns stderr followed by stdout lines matching `err|arning`, or your explicit filter. Diagnostics have an 8 KiB payload budget by default, plus small headers and the run reference. Complete retained logs remain available through `read`.

This is substring matching, not semantic error detection: `berry` matches `err`, and stack-trace continuation lines are not inferred. Default successful checks suppress all logs, including warnings; choose a filter or preview when those matter.

Compatibility shortcuts:

```sh
agent-response -- your-command
grent --filter 'warn|err' -- your-command  # successful matching logs on both streams
grent --filter '*' -- your-command        # complete retained output, original streams
grent --count -- your-command             # legacy count of err|arning stdout lines
```

Filtered success output is bounded by `--max-bytes`, except the plain wildcard shortcut, which returns all retained output. The wildcard shortcut preserves failure output and status too. Output is buffered until completion. `--failure-only` suppresses successful output and returns `ok`; `--no-stdout` disables stdout selection. Prefer explicit modes for new integrations.

## Execution and storage limits

- Commands and their output pipes have a **30-second default deadline**, configurable with `--timeout-ms`. Timeout returns 124; wrapper/storage failures return 125; Unix signals use `128 + signal`. A child can itself return these codes; JSON `incomplete` and diagnostics distinguish interrupted capture.
- Both streams are drained concurrently into private local files. Each stream has a **64 MiB default cap**, configurable with `--max-log-bytes`, up to 1 GiB. Exceeding it terminates the command and marks the run incomplete. Exact count/existence queries refuse incomplete logs.
- Timeout, cancellation, and capture failures kill the child's process group. Processes that deliberately detach into a different group can escape cleanup. Grent is intended for noninteractive commands; stdin is inherited. It is not a command sandbox.
- Store selection: `--store`, then `GRENT_STORE`, then `$XDG_STATE_HOME/grent`, then `$HOME/.local/state/grent`. Directories are private (700), files private (600); unsafe file types and links are rejected. This does not isolate logs from other processes running as you.
- Completed runs older than seven days are pruned when another command starts. Run `grent prune` explicitly, or use `--older-than-hours N`. `forget` removes a named run immediately. There is no global disk quota. Abrupt termination such as SIGKILL can leave unfinished directories that automatic pruning skips; remove those with `forget` when no longer active.
- Retained files and plain previews preserve bytes; JSON text and failure diagnostics replace invalid UTF-8. Stream order is preserved individually, not interleaved chronologically. Output content is untrusted, including terminal escapes.

## Benchmark evidence

The [committed benchmark report](benchmarks/release/REPORT.md) compares noisy output, efficient local controls, and grent. Its [raw results](benchmarks/release/results.json) include source hashes; the [CSV](benchmarks/release/runs.csv) contains each trial. [Reproduction instructions](benchmarks/README.md) are included, and CI uploads a fresh artifact.

In the committed synthetic run (three trials per variant):

| Task | Ordinary/control output tokens | Grent output tokens |
| --- | ---: | ---: |
| Successful 500-line command | 4,000; quiet shell: 2 | 2 |
| Matching-line count | 2 with local reduction | 2 |
| Preview then count | 162 with capture-once file | 252.0 with JSON/run IDs |
| Failed command with two useful diagnostics | 4,012 | 35.0 |

The preview/count side-effect check verified one original execution for both grent and capture-once, versus two for rerunning. See the report for timings and their observed spread.

The useful scope is small: compact success checks and a consistent way to retain output for follow-up queries. Quiet shell redirects achieve the same output reduction; local counts are already compact. A capture-once file provides the same avoidance of reruns. Grent packages those operations with explicit results, limits, and cleanup. JSON metadata costs extra tokens.

The suite asserts answers and side-effect counts before generating its report. Token counts describe returned stdout/stderr using `o200k_base`; they are not billed model usage and exclude prompts, tool envelopes, and reasoning. Three trials per variant support a reproducible example, not general speed claims.

## Verification

```sh
cargo fmt --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo build --release --locked
# Then run benchmarks/release_suite.py as documented above.
```

Tests cover explicit contracts, aliases, retained queries without reruns, binary/long lines, concurrent streams, timeouts, cancellation, incomplete logs, file permissions, links, corrupt metadata, expiry, and injected reader failures. One property-style test checks **10,000 seeded randomized byte streams** against an independent oracle; these are iterations, not 10,000 separate tests.

MCP transport is not implemented in this release. Agents can use the CLI through their existing command tool and inspect `grent --help`.

## License

[MIT](LICENSE).
