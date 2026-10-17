#!/bin/bash
# Run the Rust test suite, refusing a run that executed nothing.
#
# Why this exists: `cargo test <filter>` exits 0 when the filter matches no test at all
# (measured: `running 0 tests ... ok`), so a typo in a filter reports success for a suite that
# never ran. A gate that cannot decide is not a pass, which is the repository rule; this wrapper
# makes the zero-match case a failure instead of a silent success.
#
# Usage:
#   scripts/cargo-test.sh                 # the whole suite (the handoff gate)
#   scripts/cargo-test.sh --lib logger::  # a filter for iteration; zero matches still fails
#
# Every argument is passed to `cargo test` unchanged.

set -uo pipefail

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_dir=$(dirname -- "$script_dir")

output_file=$(mktemp -t oh-my-tab-test-output)
trap 'rm -f "$output_file"' EXIT

set +e
cargo test --manifest-path "$repo_dir/Cargo.toml" "$@" 2>&1 | tee "$output_file"
cargo_status=$?
set -e

# Count executed tests across every test binary that ran. `running 0 tests` is what a filter that
# matched nothing prints; a suite that ran nothing cannot be reported as passing.
executed=$(awk '/^test result:/ { for (i = 1; i <= NF; i++) if ($i == "passed;") { print $(i-1) } }' \
    "$output_file" | awk '{ total += $1 } END { print total + 0 }')

if [ "$cargo_status" -ne 0 ]; then
    exit "$cargo_status"
fi

if [ "$executed" -eq 0 ]; then
    echo "cargo-test FAILED: the run executed no test at all (a filter that matched nothing still exits 0)" >&2
    exit 1
fi

echo "cargo-test ok: $executed test(s) executed"
