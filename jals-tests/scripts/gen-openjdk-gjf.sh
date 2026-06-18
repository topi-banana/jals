#!/usr/bin/env bash
# Generate a golden corpus by running google-java-format over the OpenJDK submodule,
# producing `*.input` / `*.output` pairs under `jals-tests/sources/openjdk-gjf/`
# (gitignored — these are derivatives of GPL'd OpenJDK sources and must not be
# committed).
#
# Usage:
#   GJF_JAR=path/to/google-java-format-<ver>-all-deps.jar \
#     [SUBTREE=src] [JOBS=N] jals-tests/scripts/gen-openjdk-gjf.sh [COUNT]
#
#   GJF_JAR   (required) a google-java-format "all-deps" jar. See jals-tests/README.md
#             for where to get it.
#   SUBTREE   (optional) subtree under the submodule to walk, e.g. `src` to format only
#             the JDK library sources (what CI does). Default: the whole submodule.
#   JOBS      (optional) number of concurrent google-java-format JVMs (xargs -P).
#             Default 2 — google-java-format already parallelizes within one JVM, so
#             keep this low to avoid out-of-memory on small CI runners.
#   COUNT     (optional) cap on how many source files to consider (sorted, deterministic).
#             Default 0 = no cap (the whole subtree). Pass e.g. 500 for a quick local run.
#
# Requires `java` (google-java-format needs 21+; tested on JDK 25, which needs the
# --add-exports flags below). Files google-java-format refuses to format (parse errors,
# unsupported syntax) are skipped, so the corpus only ever contains valid input/output
# pairs. Generation is batched (a few warm JVMs format hundreds of files each) so the
# full `src/` subtree completes in minutes rather than the hours a JVM-per-file run took.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # jals-tests/
src_root="$here/sources/openjdk"
out_root="$here/sources/openjdk-gjf"
subtree="${SUBTREE:-}"
jobs="${JOBS:-2}"
count="${1:-0}"
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

# Resolve the subtree to walk. Guard its existence explicitly: under `pipefail` a failing
# `find` inside a process substitution would not fail the read loop on its own.
walk_root="$src_root${subtree:+/$subtree}"
if [[ ! -d "$walk_root" ]]; then
  echo "error: subtree not found at $walk_root (SUBTREE='$subtree')" >&2
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

# Fail fast on a corrupt/truncated jar or a missing/too-old Java: the batch below runs
# under `|| true` (google-java-format exits non-zero when it skips an unsupported file),
# which would otherwise mask a jar that cannot run at all.
if ! java "${addexports[@]}" -jar "$jar" --version >/dev/null 2>&1; then
  echo "error: '$jar' failed to run (corrupt jar, or Java too old? google-java-format needs 21+)" >&2
  exit 2
fi

# Scratch holds throwaway copies we format in place; out_tmp accumulates the final pairs
# and is swapped into place only on success, so a crash never leaves a partial corpus.
# Both live next to out_root (same filesystem) so the final `mv` is an atomic rename, and
# the `openjdk-gjf.tmp.*` names are gitignored so a crash never litters the worktree.
scratch="$(mktemp -d "$here/sources/openjdk-gjf.tmp.scratch.XXXXXX")"
out_tmp="$(mktemp -d "$here/sources/openjdk-gjf.tmp.out.XXXXXX")"
trap 'rm -rf "$scratch" "$out_tmp"' EXIT

echo "selecting .java from $walk_root (subtree='${subtree:-<all>}', count=${count:-0}) ..." >&2

# Deterministic subset: every .java under the subtree, sorted; take the first COUNT
# (COUNT=0 = all). Paths are kept relative to src_root so the corpus tree shows the
# `src/...` prefix and same-basename files (module-info.java, package-info.java) in
# different modules never collide.
selected=()
while IFS= read -r -d '' file; do
  if [[ "$count" -gt 0 && "${#selected[@]}" -ge "$count" ]]; then
    break
  fi
  rel="${file#"$src_root"/}"
  mkdir -p "$scratch/$(dirname "$rel")"
  cp "$file" "$scratch/$rel"
  selected+=("$rel")
done < <(find "$walk_root" -name '*.java' -print0 | sort -z)

echo "formatting ${#selected[@]} files with google-java-format (jobs=$jobs) ..." >&2
# Batched, warm JVMs: xargs amortizes JVM startup across ~hundreds of files per process.
# google-java-format skips files it cannot parse (printing a diagnostic) and exits 1 if
# any failed, so tolerate the non-zero exit; failures are detected below by the unchanged
# scratch copy.
find "$scratch" -name '*.java' -print0 \
  | xargs -0 -P "$jobs" -n 200 java "${addexports[@]}" -jar "$jar" --replace || true

ok=0
skipped=0
for rel in "${selected[@]}"; do
  orig="$src_root/$rel"
  formatted="$scratch/$rel"
  # google-java-format left the scratch copy byte-identical → it failed/declined (real
  # JDK src is 4-space, never already in google-java-format's 2-space style), so skip it.
  if cmp -s "$orig" "$formatted"; then
    skipped=$((skipped + 1))
    continue
  fi
  dest_dir="$out_tmp/$(dirname "$rel")"
  mkdir -p "$dest_dir"
  base="$(basename "$rel" .java)"
  cp "$orig" "$dest_dir/$base.input"
  cp "$formatted" "$dest_dir/$base.output"
  ok=$((ok + 1))
  [[ $((ok % 500)) -eq 0 ]] && echo "  ... $ok pairs" >&2
done

if [[ "$ok" -eq 0 ]]; then
  echo "error: produced 0 pairs — refusing to write an empty corpus" >&2
  exit 1
fi

# Atomic-ish swap: only replace the published corpus once generation fully succeeded.
rm -rf "$out_root"
mkdir -p "$(dirname "$out_root")"
mv "$out_tmp" "$out_root"

echo "done: $ok pairs generated, $skipped skipped (google-java-format declined) -> $out_root" >&2
