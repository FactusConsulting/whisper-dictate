#!/usr/bin/env bash
# Decide whether HEAD differs from BASE only in the whisper-dictate-app
# release version, as written by scripts/dev/bump-version.ps1.
#
# Usage: version-only-change.sh BASE HEAD
#
# Prints `true` or `false` on stdout and the reason for `false` on stderr.
# Exits non-zero when git cannot compare the two commits; callers treat that
# as `false`.
#
# `true` means the two trees are byte-identical except for these lines, where
# X is a stable (1.2.3) or release-candidate (1.2.3-rc.4) version:
#   VERSION              the single line `X`
#   src/rust/Cargo.toml  `version = "X"` inside [package]
#   src/rust/Cargo.lock  `version = "X"` right after name = "whisper-dictate-app"
#   nix/package.nix      `, version ? "X"`
# Any other changed file, line, file mode or end-of-file newline, and an empty
# diff, give `false`.
set -euo pipefail

if [ "$#" -ne 2 ]; then
  echo "usage: $0 BASE HEAD" >&2
  exit 2
fi
base=$1
head=$2

for rev in "$base" "$head"; do
  if ! git rev-parse --verify --quiet "$rev^{commit}" >/dev/null; then
    echo "unknown commit: $rev" >&2
    exit 1
  fi
done

not_version_only() {
  echo "not version-only: $*" >&2
  echo false
  exit 0
}

# Rewrites each allowed version line to a fixed placeholder, so two revisions
# of a file normalize to the same text exactly when they differ only in those
# lines. A trailing CR stays part of the line.
normalize() {
  git cat-file blob "$1:$2" | awk -v path="$2" '
    BEGIN {
      n = "(0|[1-9][0-9]*)"
      v = n "\\." n "\\." n "(-rc\\.[1-9][0-9]*)?"
    }
    {
      line = $0
      cr = ""
      if (sub(/\r$/, "", line)) cr = "\r"
      out = $0
      if (path == "VERSION") {
        if (line ~ ("^" v "$")) out = "@VERSION@" cr
      } else if (path == "src/rust/Cargo.toml") {
        if (line ~ /^\[/) section = line
        if (section == "[package]" && line ~ ("^version = \"" v "\"$")) out = "version = @VERSION@" cr
      } else if (path == "src/rust/Cargo.lock") {
        if (prev == "name = \"whisper-dictate-app\"" && line ~ ("^version = \"" v "\"$")) out = "version = @VERSION@" cr
      } else if (path == "nix/package.nix") {
        if (line ~ ("^, version [?] \"" v "\"$")) out = ", version ? @VERSION@" cr
      }
      prev = line
      print out
    }'
}

raw=$(git diff --raw --no-color --no-renames "$base" "$head" --)
if [ -z "$raw" ]; then
  not_version_only "no changes"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

while IFS= read -r entry; do
  # :<old mode> <new mode> <old blob> <new blob> <status><TAB><path>
  path=${entry#*$'\t'}
  meta=${entry%%$'\t'*}
  read -r old_mode new_mode _ _ status <<<"${meta#:}"
  case "$path" in
    VERSION | nix/package.nix | src/rust/Cargo.toml | src/rust/Cargo.lock) ;;
    *) not_version_only "$path changed" ;;
  esac
  if [ "$status" != M ] || [ "$old_mode" != "$new_mode" ]; then
    not_version_only "$path status $status, mode $old_mode -> $new_mode"
  fi
  numstat=$(git diff --numstat --no-color --no-renames "$base" "$head" -- "$path")
  if [[ $numstat == -$'\t'-* ]]; then
    not_version_only "$path is binary"
  fi
  patch=$(git diff --no-color --no-ext-diff --no-textconv --no-renames "$base" "$head" -- "$path")
  if [[ $patch == *$'\n\\ No newline at end of file'* ]]; then
    not_version_only "$path changes or lacks its final newline"
  fi
  normalize "$base" "$path" >"$tmp/base"
  normalize "$head" "$path" >"$tmp/head"
  if ! cmp -s "$tmp/base" "$tmp/head"; then
    not_version_only "$path changes more than the release version line"
  fi
done <<<"$raw"

echo "version-only: $base..$head changes only the release version lines" >&2
echo true
