#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
INPUT="${1:-$ROOT_DIR/examples/llvm/place_safepoints_input.ll}"
BC="/tmp/mlisp-place-safepoints.bc"

llvm-as "$INPUT" -o "$BC"
opt -passes="function(place-safepoints),rewrite-statepoints-for-gc" -disable-output "$BC"
