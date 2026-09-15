#!/usr/bin/env python3
"""Synthetic correctness + output-volume benchmark. No transcript input or network use."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import tempfile
import time
import re

import tiktoken

ROOT = Path(__file__).resolve().parents[1]
FIXTURE = '''import pathlib, sys
if len(sys.argv) > 1 and sys.argv[1] != "fail":
    counter = pathlib.Path(sys.argv[1])
    counter.write_text(str(int(counter.read_text()) + 1) if counter.exists() else "1")
for i in range(500):
    print(f"item {i:05d} status=ok")
if "fail" in sys.argv:
    print("ERROR useful stdout failure detail")
    print("useful stderr failure detail", file=sys.stderr)
    sys.exit(7)
'''
CONTROL = '''import pathlib, subprocess, sys
mode, path = sys.argv[1:3]
if mode == "read":
    data = pathlib.Path(path).read_text()
else:
    result = subprocess.run(sys.argv[3:], capture_output=True, text=True)
    if result.returncode:
        sys.stdout.write(result.stdout); sys.stderr.write(result.stderr); sys.exit(result.returncode)
    data = result.stdout
    if mode == "capture": pathlib.Path(path).write_text(data)
if mode in ("preview", "capture"):
    print("".join(data.splitlines(keepends=True)[:20]), end="")
elif mode == "exists": print("true" if "item 0000" in data else "false")
else: print(sum("item 0000" in line for line in data.splitlines()))
'''


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--binary", type=Path, default=ROOT / "target/release/grent")
    p.add_argument("--output", type=Path, default=ROOT / "benchmarks/release")
    p.add_argument("--trials", type=int, default=3)
    a = p.parse_args()
    if a.trials < 3:
        p.error("at least three trials are required")
    binary = a.binary.resolve()
    encoding = tiktoken.get_encoding("o200k_base")
    expected_preview = "".join(f"item {i:05d} status=ok\n" for i in range(20))
    rows = []
    checks = []
    with tempfile.TemporaryDirectory(prefix="grent-release-") as temp:
        temp = Path(temp)
        fixture = temp / "fixture.py"
        fixture.write_text(FIXTURE)
        control = temp / "control.py"
        control.write_text(CONTROL)
        store = temp / "store"
        env = dict(os.environ, GRENT_STORE=str(store))
        command = [sys.executable, str(fixture)]

        def invoke(argv):
            start = time.perf_counter_ns()
            r = subprocess.run(argv, capture_output=True, env=env, timeout=40)
            ms = (time.perf_counter_ns() - start) / 1e6
            return r, ms

        def record(group, variant, trial, runs, command_executions=1):
            # Normalize only opaque identifiers/paths, not the measured output itself.
            out = b"".join(r.stdout for r, _ in runs).decode("utf-8", "replace")
            err = b"".join(r.stderr for r, _ in runs).decode("utf-8", "replace")
            def normalized(s):
                s = s.replace(str(temp), "<TEMP>")
                return re.sub(r'("run_id"\s*:\s*")[^"]+(\")', r'\1<RUN_ID>\2', s)
            chunks = [stream.decode("utf-8", "replace") for r, _ in runs for stream in (r.stdout, r.stderr)]
            rows.append(dict(group=group, variant=variant, trial=trial,
                elapsed_ms=round(sum(ms for _, ms in runs), 3),
                stdout_bytes=len(out.encode()), stderr_bytes=len(err.encode()),
                output_tokens=sum(len(encoding.encode(chunk)) for chunk in chunks),
                normalized_output_tokens=sum(len(encoding.encode(normalized(chunk))) for chunk in chunks),
                command_executions=command_executions,
                exit_codes=[r.returncode for r, _ in runs]))

        def grent(*args):
            return [str(binary), *args]

        for trial in range(1, a.trials + 1):
            r = invoke(command)
            assert r[0].returncode == 0 and len(r[0].stdout.splitlines()) == 500
            record("successful_check", "ordinary", trial, [r])
            r = invoke(["sh", "-c", '"$@" >/dev/null && printf "ok\\n"', "sh", *command])
            assert r[0].returncode == 0 and r[0].stdout == b"ok\n"
            record("successful_check", "shell_quiet", trial, [r])
            r = invoke(grent("check", "--", *command))
            assert r[0].returncode == 0 and r[0].stdout.strip() == b"ok"
            record("successful_check", "grent", trial, [r])

            for mode, expected in (("count", b"10"), ("exists", b"true")):
                r = invoke([sys.executable, str(control), mode, "unused", *command])
                assert r[0].returncode == 0 and r[0].stdout.strip() == expected
                record(mode, "local_reduction", trial, [r])
                r = invoke(grent(mode, "--filter", "item 0000", "--", *command))
                assert r[0].returncode == 0 and r[0].stdout.strip() == expected
                record(mode, "grent", trial, [r])

            for variant in ("rerun", "capture_once", "grent"):
                counter = temp / f"counter-{trial}-{variant}"
                side_command = [*command, str(counter)]
                if variant == "grent":
                    first = invoke(grent("preview", "--json", "--lines", "20", "--", *side_command))
                    assert first[0].returncode == 0, first[0].stderr
                    data = json.loads(first[0].stdout)
                    assert data["exit_code"] == 0 and data["output"] == expected_preview
                    assert data["truncated"] is True
                    second = invoke(grent("read", data["run_id"], "--mode", "count", "--stream", "stdout", "--filter", "item 0000", "--json"))
                    result = json.loads(second[0].stdout)
                    assert second[0].returncode == 0 and result["count"] == 10
                    expected_runs = 1
                else:
                    capture = temp / f"capture-{trial}.txt"
                    first = invoke([sys.executable, str(control), "capture" if variant == "capture_once" else "preview", str(capture), *side_command])
                    assert first[0].returncode == 0 and first[0].stdout.decode() == expected_preview
                    second = invoke([sys.executable, str(control), "read" if variant == "capture_once" else "count", str(capture), *side_command])
                    assert second[0].returncode == 0 and second[0].stdout.strip() == b"10"
                    expected_runs = 1 if variant == "capture_once" else 2
                assert counter.read_text() == str(expected_runs)
                record("preview_then_count", variant, trial, [first, second], expected_runs)

            for variant in ("ordinary", "grent"):
                argv = [*command, "fail"]
                r = invoke(argv if variant == "ordinary" else grent("check", "--", *argv))
                assert r[0].returncode == 7
                combined = r[0].stdout + r[0].stderr
                assert b"ERROR useful stdout failure detail" in combined
                assert b"useful stderr failure detail" in combined
                record("failure_diagnostics", variant, trial, [r])
        checks = ["500-line fixture succeeds", "check returns ok", "count returns 10 matching lines", "exists returns true", "preview returns 20 lines and truncation marker", "retained query returns count 10", "retained and file-control commands execute once; rerun baseline executes twice", "exit code 7 and both failure diagnostic streams survive"]

    a.output.mkdir(parents=True, exist_ok=True)
    source_files = sorted([ROOT / "Cargo.toml", ROOT / "Cargo.lock", *ROOT.glob("src/**/*.rs"), Path(__file__)])
    fingerprints = {str(x.relative_to(ROOT)): sha(x) for x in source_files}
    metadata = dict(schema_version=1, trials=a.trials, python=platform.python_version(),
        platform=f"{platform.system()} {platform.release()} {platform.machine()}",
        tokenizer=f"tiktoken {tiktoken.__version__} / o200k_base",
        binary_sha256=sha(binary), source_sha256=fingerprints,
        rustc=subprocess.run(["rustc", "--version"], capture_output=True, text=True, check=True).stdout.strip(),
        correctness_checks=checks,
        measurement="Wall time includes process startup. Tokens count returned stdout/stderr independently, not tool envelopes, prompts, reasoning, cached input, or billed usage. Raw token counts include opaque run IDs; normalized counts replace them.")
    (a.output / "results.json").write_text(json.dumps(dict(metadata=metadata, runs=rows), indent=2) + "\n")
    with (a.output / "runs.csv").open("w", newline="") as f:
        writer = csv.DictWriter(f, fieldnames=list(rows[0]), lineterminator="\n")
        writer.writeheader(); writer.writerows(rows)
    lines = ["# Synthetic release benchmark", "", "Generated by `benchmarks/release_suite.py`; all correctness assertions passed.", "", "## Reproduce", "", "```sh", "cargo build --release", "python3 -m venv /tmp/grent-bench", "/tmp/grent-bench/bin/pip install -r benchmarks/requirements.txt", "/tmp/grent-bench/bin/python benchmarks/release_suite.py", "```", "", "The tokenizer may download its public vocabulary on first use. The benchmark itself uses only generated local fixtures and does not read transcripts or run historical commands.", "", "## Results", "", "Each row summarizes three trials (or the configured trial count). Time is median wall-clock milliseconds with the observed min–max; tokens are mean raw returned-output tokens. Two-step tasks include both calls.", "", "| Task | Variant | Median ms (min–max) | Mean output tokens | Command executions |", "|---|---|---:|---:|---:|"]
    for group, variant in dict.fromkeys((r["group"], r["variant"]) for r in rows):
        subset = [r for r in rows if (r["group"], r["variant"]) == (group, variant)]
        times = [r["elapsed_ms"] for r in subset]
        tokens = statistics.mean(r["output_tokens"] for r in subset)
        lines.append(f"| {group} | {variant} | {statistics.median(times):.2f} ({min(times):.2f}–{max(times):.2f}) | {tokens:.1f} | {subset[0]['command_executions']} |")
    lines += ["", "## Interpretation", "", "- **Narrow use case:** when a successful command's logs are irrelevant, `check` preserves its success signal while reducing returned output. A shell redirect achieves the same reduction.", "- **Counts and existence:** local reduction already saves tokens. Grent provides an explicit interface; it does not improve on an equally concise pipeline's information content.", "- **Follow-up without rerunning:** retained output supports preview then a different query with exactly one original command execution. The side-effect counter verifies this. A capture-once local-file control provides the same capability; grent packages retention and retrieval.", "- **Failures:** the fixture's exit status and useful messages from stdout and stderr remain available. This is an assertion about this fixture, not proof that a heuristic captures every possible diagnostic.", "- JSON metadata and run IDs add output tokens. These are output-volume measurements, **not actual model usage or billing**, and do not include the tokens needed to request a command.", "- Three trials and a single synthetic fixture are not enough for general performance claims. Startup, disk caching, OS scheduling, and retained-file writes affect timings. No speed thresholds are asserted.", "", "## Evidence", "", "[Machine-readable results and source hashes](results.json) · [Per-trial measurements](runs.csv)", "", "Correctness checks:", "", *[f"- {c}." for c in checks], ""]
    (a.output / "REPORT.md").write_text("\n".join(lines))
    print(f"PASS: {len(rows)} measured task trials; artifact generated in {a.output.name}/")


if __name__ == "__main__":
    main()
