#!/usr/bin/env bash
set -euo pipefail

# We resolve llvm-cov/profdata via nix so coverage works in this environment
# even when rustup-managed llvm-tools-preview is unavailable.
LLVM_BIN="$(nix shell nixpkgs#llvmPackages_21.llvm -c sh -c 'dirname "$(command -v llvm-cov)"')"

export LLVM_COV="$LLVM_BIN/llvm-cov"
export LLVM_PROFDATA="$LLVM_BIN/llvm-profdata"

# Excluding src/main.rs keeps the quality gate focused on testable library logic.
# The binary entrypoint is intentionally minimal and mostly wiring.
#
# Region coverage is lower than line/function coverage because tracing macros and
# compiler-generated regions create bookkeeping paths that are not meaningful
# to exercise directly in tests. Keep this threshold strict but realistic.
cargo llvm-cov \
  --lib \
  --tests \
  --ignore-filename-regex 'src/main.rs' \
  --summary-only \
  --fail-under-lines 99 \
  --fail-under-functions 100 \
  --fail-under-regions 97
