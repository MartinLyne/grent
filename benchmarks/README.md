# Release benchmark

The suite generates local fixtures, asserts the returned answers and command execution counts, and writes a reviewable artifact. It does not read transcripts or replay historical commands.

```sh
cargo build --release --locked
python3 -m venv /tmp/grent-bench
/tmp/grent-bench/bin/pip install -r benchmarks/requirements.txt
/tmp/grent-bench/bin/python benchmarks/release_suite.py
```

See the committed [report](release/REPORT.md), [raw results and source hashes](release/results.json), and [per-trial CSV](release/runs.csv). CI generates a fresh downloadable artifact on every push.

The default is three trials of twelve variants (36 task trials, including two-step queries). Use `--trials N`, `--binary PATH`, or `--output DIR` to change the run. The tokenizer may download its public vocabulary on first use.

Tokens measure returned stdout/stderr using `o200k_base`, not actual model usage or billing. Timings include process startup. Comparisons include quiet shell output, local reductions, and capture-once files; these controls matter when interpreting any benefit.
