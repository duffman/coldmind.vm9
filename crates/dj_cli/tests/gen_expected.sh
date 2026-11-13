#!/bin/sh
# Regenerates tests/cases/*.out from Node (the reference engine).
# Each case runs as a strict ES module (.mts), matching djungle's module semantics.
# Usage: crates/dj_cli/tests/gen_expected.sh [case.ts ...]
set -eu
NODE="${NODE:-node}"
dir="$(cd "$(dirname "$0")/cases" && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
[ "$#" -gt 0 ] || set -- "$dir"/*.ts
for f in "$@"; do
  base="$(basename "${f%.ts}")"
  cp "$f" "$tmp/$base.mts"
  "$NODE" --experimental-transform-types --no-warnings "$tmp/$base.mts" > "$dir/$base.out"
  echo "wrote $dir/$base.out"
done
