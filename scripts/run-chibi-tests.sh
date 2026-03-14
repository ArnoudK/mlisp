#!/usr/bin/env bash

set -euo pipefail

ROOT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
CHIBI_BASIC_DIR="$ROOT_DIR/chibi-scheme/tests/basic"

usage() {
  cat <<'EOF'
Usage: scripts/run-chibi-tests.sh [--list] [case ...]

Run Chibi's basic test fixtures against mlisp and compare stdout with the
upstream .res files.

Options:
  --list  Print all available fixture names
  -h      Show this help text

Examples:
  scripts/run-chibi-tests.sh
  scripts/run-chibi-tests.sh test06-letrec test07-mutation
EOF
}

list_cases() {
  printf 'Available fixtures:\n'
  (
    cd "$CHIBI_BASIC_DIR"
    for file in *.scm; do
      printf '  %s\n' "${file%.scm}"
    done
  )
}

normalize_case_name() {
  local value=$1
  value=${value##*/}
  value=${value%.scm}
  printf '%s\n' "$value"
}

if [[ ! -d "$CHIBI_BASIC_DIR" ]]; then
  printf 'missing Chibi basic tests at %s\n' "$CHIBI_BASIC_DIR" >&2
  printf 'initialize the submodule first: git submodule update --init --recursive\n' >&2
  exit 1
fi

declare -a requested_cases=()

while (($# > 0)); do
  case "$1" in
    --list)
      list_cases
      exit 0
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    -*)
      printf 'unknown option: %s\n' "$1" >&2
      usage >&2
      exit 1
      ;;
    *)
      requested_cases+=("$(normalize_case_name "$1")")
      shift
      ;;
  esac
done

declare -a cases=()
if ((${#requested_cases[@]} > 0)); then
  cases=("${requested_cases[@]}")
else
  while IFS= read -r file; do
    cases+=("${file%.scm}")
  done < <(
    cd "$CHIBI_BASIC_DIR"
    printf '%s\n' *.scm | sort
  )
fi

for case_name in "${cases[@]}"; do
  if [[ ! -f "$CHIBI_BASIC_DIR/$case_name.scm" ]]; then
    printf 'unknown Chibi fixture: %s\n' "$case_name" >&2
    exit 1
  fi
done

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

build_log="$tmp_dir/mlisp-build.stderr"
if ! (
  cd "$ROOT_DIR"
  cargo build --quiet
) >/dev/null 2>"$build_log"; then
  printf 'mlisp build failed; cannot run Chibi fixtures\n' >&2
  cat "$build_log" >&2
  exit 1
fi

failures=0
declare -a failed_cases=()

for case_name in "${cases[@]}"; do
  scm_file="$CHIBI_BASIC_DIR/$case_name.scm"
  expected_file="$CHIBI_BASIC_DIR/$case_name.res"
  stdout_file="$tmp_dir/$case_name.stdout"
  stderr_file="$tmp_dir/$case_name.stderr"
  diff_file="$tmp_dir/$case_name.diff"

  set +e
  (
    cd "$ROOT_DIR"
    ./target/debug/mlisp run "$scm_file"
  ) >"$stdout_file" 2>"$stderr_file"
  status=$?
  set -e

  stderr_ok=false
  if [[ ! -s "$stderr_file" ]]; then
    stderr_ok=true
  elif grep -Eq '^codegen error: program exited with status [0-9]+$' "$stderr_file"; then
    stderr_ok=true
  fi

  if diff -u "$expected_file" "$stdout_file" >"$diff_file" && [[ "$stderr_ok" == true ]]; then
    printf '[PASS] %s\n' "$case_name"
    continue
  fi

  failures=$((failures + 1))
  failed_cases+=("$case_name")
  printf '[FAIL] %s\n' "$case_name"

  if [[ -s "$diff_file" ]]; then
    cat "$diff_file"
  fi

  if [[ "$stderr_ok" == false ]]; then
    printf '%s\n' 'stderr:'
    cat "$stderr_file"
  elif [[ $status -ne 0 ]]; then
    printf 'note: mlisp exited with status %d after producing matching stdout\n' "$status"
  fi
done

if [[ $failures -ne 0 ]]; then
  printf '\nfailed fixtures (%d):\n' "$failures"
  printf '  %s\n' "${failed_cases[@]}"
  exit 1
fi

printf '\nall %d fixture(s) passed\n' "${#cases[@]}"
