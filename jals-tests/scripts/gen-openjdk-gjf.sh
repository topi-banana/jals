#!/usr/bin/env bash
# Generate a golden corpus by running google-java-format over a deterministic subset
# of the OpenJDK submodule, producing `*.input` / `*.output` pairs under
# `jals-tests/sources/openjdk-gjf/` (gitignored — these are derivatives of GPL'd
# OpenJDK sources and must not be committed).
#
# Usage:
#   GJF_JAR=path/to/google-java-format-<ver>-all-deps.jar \
#     jals-tests/scripts/gen-openjdk-gjf.sh [COUNT]
#
#   COUNT   number of source files to format (default 500)
#
# Requires `java` (17+; tested on JDK 25, which needs the --add-exports flags below)
# and a google-java-format "all-deps" jar. See jals-tests/README.md for where to get
# the jar. Files google-java-format refuses to format (parse errors, unsupported
# syntax) are skipped, so the corpus only ever contains valid input/output pairs.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # jals-tests/
src_root="$here/sources/openjdk"
out_root="$here/sources/openjdk-gjf"
count="${1:-500}"
jar="${GJF_JAR:-}"

if [[ -z "$jar" || ! -f "$jar" ]]; then
  echo "error: set GJF_JAR to a google-java-format all-deps jar (got: '${jar:-unset}')" >&2
  exit 2
fi
if [[ ! -d "$src_root" ]]; then
  echo "error: OpenJDK submodule not checked out at $src_root" >&2
  echo "       run: git submodule update --init --depth 1 jals-tests/sources/openjdk" >&2
  exit 2
fi

# JDK 16+ closed off javac internals; google-java-format needs them re-exported.
addexports=(
  --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.main=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED
)

echo "generating up to $count goldens from $src_root -> $out_root" >&2
rm -rf "$out_root"

ok=0
skipped=0
# Deterministic subset: every .java file, sorted, take the first COUNT.
while IFS= read -r file; do
  [[ "$ok" -ge "$count" ]] && break
  rel="${file#"$src_root"/}"
  dest_dir="$out_root/$(dirname "$rel")"
  formatted="$(java "${addexports[@]}" -jar "$jar" "$file" 2>/dev/null)" || {
    skipped=$((skipped + 1))
    continue
  }
  mkdir -p "$dest_dir"
  cp "$file" "$dest_dir/$(basename "$rel" .java).input"
  printf '%s' "$formatted" >"$dest_dir/$(basename "$rel" .java).output"
  ok=$((ok + 1))
  [[ $((ok % 50)) -eq 0 ]] && echo "  ... $ok formatted" >&2
done < <(find "$src_root" -name '*.java' | sort)

echo "done: $ok pairs generated, $skipped skipped (google-java-format declined)" >&2
