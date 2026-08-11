#!/usr/bin/env bash
# Generate the `jals-compile` corpus by running the pinned `javac` over OpenJDK's own compiler
# tests, keeping every file javac compiles **on its own** and recording why it declined the rest.
#
#   jals-tests/scripts/gen-javac-corpus.sh [COUNT]
#
#   COUNT     (optional) cap on how many candidate files to consider (sorted, deterministic).
#             Default 0 = no cap. Pass e.g. 400 for a quick local sample.
#   SUBTREE   (optional) subtree of the OpenJDK submodule to walk.
#             Default: test/langtools/tools/javac
#   JOBS      (optional) concurrent javac processes (xargs -P). Default: the CPU count.
#   JAVAC_TIMEOUT
#             (optional) seconds one javac may take before the file is dropped. Default 60.
#
# Output, under `jals-tests/sources/javac-langtools/` (gitignored — these are derivatives of GPL'd
# OpenJDK sources and must not be committed):
#
#   <rel>/<Base>.java          the source, verbatim
#   <rel>/<Base>.expected/     the class files javac produced from it
#   SKIPPED.tsv                <rel> \t <why javac declined it alone>
#
# `expected/` is what a future run-equivalence rung diffs against. Nothing reads it yet; it is
# written now because regenerating the corpus to obtain it later costs the whole generation pass.
#
# # Why javac is the oracle
#
# There is no ready-made `.java` → expected `.class` corpus. OpenJDK's javac tests are jtreg-driven
# behaviour and diagnostic tests: a fifth are `@compile/fail` (deliberately invalid Java, which
# measures nothing for a compiler that never checks), and a third are auxiliary sources that only
# mean something beside a sibling. Running javac over each candidate alone is what separates the
# files a single-file compiler could be expected to handle from the ones it could not — and the
# ones it could not are recorded as out of scope rather than counted as failures.
#
# # The pin
#
# JAVAC_PIN=25. The measurement depends on the JDK twice — javac decides the corpus and its
# `ct.sym` is the classpath the harness resolves against — so the generator refuses a different
# release rather than quietly producing a corpus the report would mislabel.
set -euo pipefail

# Keep in step with `jals_tests::compile::JAVAC_PIN` and `JAVAC_VERSION` in ci.yml; the harness's
# own tests fail when these drift apart.
JAVAC_PIN=25

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # jals-tests/
count="${1:-0}"
subtree="${SUBTREE:-test/langtools/tools/javac}"
jobs="${JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
src_root="$here/sources/openjdk"
out_root="$here/sources/javac-langtools"

if [[ ! -d "$src_root" ]]; then
  echo "error: OpenJDK submodule not checked out at $src_root" >&2
  echo "       run: git submodule update --init --depth 1 jals-tests/sources/openjdk" >&2
  exit 2
fi

walk_root="$src_root/$subtree"
if [[ ! -d "$walk_root" ]]; then
  echo "error: subtree not found at $walk_root (SUBTREE='$subtree')" >&2
  exit 2
fi

if ! command -v javac >/dev/null 2>&1; then
  echo "error: no javac on PATH — the corpus is defined by what javac compiles" >&2
  exit 2
fi

# `javac -version` prints e.g. `javac 25.0.1`; the feature release is the part before the dot.
javac_release="$(javac -version 2>&1 | awk '{print $2}' | cut -d. -f1)"
if [[ "$javac_release" != "$JAVAC_PIN" ]]; then
  echo "error: javac $javac_release is on PATH, but the corpus is pinned to JDK $JAVAC_PIN" >&2
  echo "       a corpus generated under one release and scored under another is two" >&2
  echo "       measurements wearing one number; install JDK $JAVAC_PIN or bump the pin" >&2
  exit 2
fi

# Scratch holds per-file javac output and out_tmp accumulates the corpus, swapped into place only
# on success so a crash never leaves a partial corpus. Both live beside out_root (same filesystem)
# so the final move is a rename, and the `.tmp.*` names are gitignored.
scratch="$(mktemp -d "$here/sources/javac-langtools.tmp.scratch.XXXXXX")"
out_tmp="$(mktemp -d "$here/sources/javac-langtools.tmp.out.XXXXXX")"
trap 'rm -rf "$scratch" "$out_tmp"' EXIT

echo "selecting .java from $walk_root (count=${count}) ..." >&2

# Deterministic candidate set: every .java under the subtree, sorted, minus the ones whose jtreg
# header says they are negative tests. Those are excluded outright rather than left to fail under
# javac: `jals-javac` never checks, so scoring a file whose whole purpose is to be rejected would
# quietly turn this harness into a checker.
candidates=()
while IFS= read -r -d '' file; do
  if [[ "$count" -gt 0 && "${#candidates[@]}" -ge "$count" ]]; then
    break
  fi
  if grep -qE '@compile/fail|@compile/ref|-XDrawDiagnostics' "$file"; then
    continue
  fi
  candidates+=("${file#"$src_root"/}")
done < <(find "$walk_root" -name '*.java' -print0 | sort -z)

echo "compiling ${#candidates[@]} candidates with javac $javac_release (jobs=$jobs) ..." >&2

# One javac per candidate, in parallel. `-proc:none` keeps an annotation processor on the corpus's
# own source path from running, `-nowarn` keeps the diagnostics to errors, and the first error line
# is kept as the out-of-scope reason.
#
# Under `timeout`, because this suite contains tests that make **javac itself** hang: it is a
# compiler's regression suite, so some of it exists to push type inference to its limits
# (`switchexpr/ExpressionSwitchComplexIntersectionTest.java` is one — a complex intersection type
# that javac has never returned from in any run here). One such file with no timeout stalls the
# whole generation indefinitely, which in CI is a job that burns its budget and produces nothing.
# A file javac cannot compile in a minute is out of scope by the same rule as one it rejects.
compile_one() {
  local rel="$1" scratch="$2" src_root="$3"
  local slug out log status
  slug="$(printf '%s' "$rel" | tr '/' '_')"
  out="$scratch/$slug"
  log="$scratch/$slug.log"
  mkdir -p "$out"
  status=0
  timeout "${JAVAC_TIMEOUT:-60}" javac -nowarn -proc:none -d "$out" "$src_root/$rel" >"$log" 2>&1 ||
    status=$?
  if [[ "$status" -eq 0 ]]; then
    printf 'OK\t%s\n' "$rel"
  elif [[ "$status" -eq 124 ]]; then
    # `timeout`'s own exit code. Reported as its own reason rather than folded into the diagnostic
    # buckets: nothing was diagnosed, javac simply did not finish.
    printf 'SKIP\t%s\t%s\n' "$rel" "javac did not finish in ${JAVAC_TIMEOUT:-60}s"
    rm -rf "$out"
  else
    # javac's first error line, reduced to its message: `Foo.java:3: error: cannot find symbol`
    # becomes `cannot find symbol`, so the reasons bucket instead of listing every file twice.
    local reason
    reason="$(grep -m1 -oE '(error|エラー): .*' "$log" | sed -E 's/^[^:]*: //' | cut -c1-80)"
    printf 'SKIP\t%s\t%s\n' "$rel" "${reason:-javac declined it}"
    rm -rf "$out"
  fi
  rm -f "$log"
}
export -f compile_one

printf '%s\0' "${candidates[@]}" |
  xargs -0 -P "$jobs" -I{} bash -c 'compile_one "$@"' _ {} "$scratch" "$src_root" >"$scratch.results" || true

ok=0
skipped=0
: >"$out_tmp/SKIPPED.tsv"
while IFS=$'\t' read -r verdict rel reason; do
  case "$verdict" in
  OK)
    slug="$(printf '%s' "$rel" | tr '/' '_')"
    dest="$out_tmp/$(dirname "$rel")"
    base="$(basename "$rel" .java)"
    mkdir -p "$dest"
    cp "$src_root/$rel" "$dest/$base.java"
    # javac's own output, kept for a future run-equivalence rung.
    mkdir -p "$dest/$base.expected"
    cp -r "$scratch/$slug/." "$dest/$base.expected/"
    ok=$((ok + 1))
    [[ $((ok % 250)) -eq 0 ]] && echo "  ... $ok cases" >&2
    ;;
  SKIP)
    printf '%s\t%s\n' "$rel" "$reason" >>"$out_tmp/SKIPPED.tsv"
    skipped=$((skipped + 1))
    ;;
  *) ;;
  esac
done < <(sort "$scratch.results")

if [[ "$ok" -eq 0 ]]; then
  echo "error: produced 0 cases — refusing to write an empty corpus" >&2
  exit 1
fi

# Only replace the published corpus once generation fully succeeded.
rm -rf "$out_root"
mkdir -p "$(dirname "$out_root")"
mv "$out_tmp" "$out_root"

echo "done: $ok cases, $skipped out of scope -> $out_root" >&2
