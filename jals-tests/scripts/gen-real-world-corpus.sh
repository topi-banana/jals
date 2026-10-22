#!/usr/bin/env bash
# Clone the real-world lint corpus into `jals-tests/sources/real-world/`.
#
# The corpus is generated, never committed — the projects are other people's code and a checkout of
# them is not this repository's to carry. What *is* carried is the registry
# (`corpus/real-world.toml`), which pins each project to a tag or a commit so two runs are one
# measurement, and `real-world-lint.tsv`, which pins what the analysis found there.
#
# Offline afterwards: nothing in the measurement fetches anything, so this is the only step that
# needs the network.
#
# Usage: gen-real-world-corpus.sh
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
registry="$here/corpus/real-world.toml"
out="$here/sources/real-world"

mkdir -p "$out"

fetch() {
  [ -n "${name:-}" ] && [ -n "${url:-}" ] && [ -n "${ref:-}" ] || return 0
  destination="$out/$name"
  if [ -d "$destination/.git" ]; then
    echo "  $name: already generated"
  else
    echo "  $name: cloning $url @ $ref"
    # A tag's name and a commit's are both `--branch`-able only sometimes, so a shallow clone at
    # the ref is tried first and a full clone plus checkout is what falls back to.
    git clone --quiet --depth 1 --branch "$ref" "$url" "$destination" 2>/dev/null || {
      rm -rf "$destination"
      git clone --quiet "$url" "$destination"
      git -C "$destination" checkout --quiet "$ref"
    }
  fi
}

# An entry's fields follow its `[[project]]` marker, so one is complete when the *next* marker
# arrives — or at end of file, which is the one the loop below has to remember to flush.
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    'name = "'*) name="${line#name = \"}"; name="${name%\"}" ;;
    'url = "'*) url="${line#url = \"}"; url="${url%\"}" ;;
    'ref = "'*) ref="${line#ref = \"}"; ref="${ref%\"}" ;;
    '[[project]]')
      fetch
      name=""; url=""; ref=""
      ;;
  esac
done < "$registry"
fetch

echo "corpus generated under $out"
