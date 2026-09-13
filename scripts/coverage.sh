#!/usr/bin/env bash
# Measure workspace test coverage and enforce a minimum line-coverage
# threshold. This is the single entrypoint used by both humans and CI
# (.github/workflows/ci.yml), so the local and CI code paths are identical.
#
# Tooling: cargo-llvm-cov (LLVM source-based coverage). Install with
#   cargo install cargo-llvm-cov --locked
# and provide the LLVM coverage tools. On a rustup toolchain:
#   rustup component add llvm-tools-preview
# On a Homebrew Rust toolchain (no rustup), point cargo-llvm-cov at Homebrew's
# LLVM instead:
#   export LLVM_COV=/opt/homebrew/opt/llvm/bin/llvm-cov
#   export LLVM_PROFDATA=/opt/homebrew/opt/llvm/bin/llvm-profdata
#
# Usage:
#   scripts/coverage.sh              # summary + lcov report, enforce threshold
#   scripts/coverage.sh --html       # also write an HTML report
#   scripts/coverage.sh --no-fail    # report only; never fail on low coverage
#   scripts/coverage.sh -h | --help
#
# Environment:
#   COVERAGE_MIN   minimum line-coverage percent (default below).
#
# Outputs (under target/llvm-cov/):
#   target/llvm-cov/lcov.info      LCOV report (uploaded as a CI artifact)
#   target/llvm-cov/html/          HTML report (only with --html)
#
# Exit code is non-zero when coverage is under COVERAGE_MIN (unless --no-fail).

set -euo pipefail

cd "$(dirname "$0")/.."

# The floor — the single source of truth for the coverage gate. CI calls this
# script without overriding COVERAGE_MIN. It matches prospero's floor; the
# baseline at the workspace scaffold (#8) measured 100%.
COVERAGE_MIN="${COVERAGE_MIN:-85}"

DO_HTML=0
DO_FAIL=1

for arg in "$@"; do
    case "$arg" in
        --html)    DO_HTML=1 ;;
        --no-fail) DO_FAIL=0 ;;
        -h|--help)
            sed -n '2,28p' "$0"
            exit 0
            ;;
        *)
            echo "unknown flag: $arg" >&2
            exit 2
            ;;
    esac
done

if ! cargo llvm-cov --version >/dev/null 2>&1; then
    cat >&2 <<'MSG'
error: cargo-llvm-cov is not installed.

  cargo install cargo-llvm-cov --locked
  rustup component add llvm-tools-preview   # or set LLVM_COV / LLVM_PROFDATA

See https://github.com/taiki-e/cargo-llvm-cov for details.
MSG
    exit 127
fi

run() {
    echo "==> $*"
    "$@"
}

# The daemon's `main` (the long-running entrypoint) is excluded from the
# coverage denominator, as in prospero: logic belongs in library code where
# tests reach it. The CLI `main` is intentionally NOT excluded.
IGNORE_REGEX='crates/daemon/src/main\.rs'

OUT_DIR="target/llvm-cov"
LCOV_PATH="$OUT_DIR/lcov.info"

# cargo-llvm-cov does not create the parent dir for a custom --output-path.
mkdir -p "$OUT_DIR"

echo "coverage floor: ${COVERAGE_MIN}% line coverage (COVERAGE_MIN)"

# Gather coverage once and write the LCOV report. The threshold is enforced as
# a separate final step, so the report exists even when the gate fails.
run cargo llvm-cov --workspace \
    --ignore-filename-regex "$IGNORE_REGEX" --lcov --output-path "$LCOV_PATH"

if [[ $DO_HTML -eq 1 ]]; then
    run cargo llvm-cov report --ignore-filename-regex "$IGNORE_REGEX" \
        --html --output-dir "$OUT_DIR"
    echo "HTML report: $OUT_DIR/html/index.html"
fi

echo
echo "coverage report written to $LCOV_PATH"

# Gate last: reuses the gathered data to print the summary table and fail when
# line coverage is under the floor.
if [[ $DO_FAIL -eq 1 ]]; then
    run cargo llvm-cov report --summary-only \
        --ignore-filename-regex "$IGNORE_REGEX" --fail-under-lines "$COVERAGE_MIN"
fi
