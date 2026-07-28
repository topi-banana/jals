#!/usr/bin/env bash
# Generate a golden corpus by running one native Java formatter over the OpenJDK
# submodule, producing `*.input` / `*.output` pairs under
# `jals-tests/sources/openjdk-<target>/` (gitignored — these are derivatives of GPL'd
# OpenJDK sources and must not be committed).
#
# Usage:
#   <tool env var> [SUBTREE=src] [JOBS=N] \
#     jals-tests/scripts/gen-openjdk-corpus.sh <target> [COUNT]
#
#   target    one of: gjf | palantir | eclipse | intellij. Selects both the formatter
#             invoked and the output directory (`sources/openjdk-<target>`).
#   SUBTREE   (optional) subtree under the submodule to walk, e.g. `src` to format only
#             the JDK library sources (what CI does). Default: the whole submodule.
#   JOBS      (optional) number of concurrent formatter processes (xargs -P). Default 2 —
#             the JVM-backed formatters already parallelize internally, so keep this low
#             to avoid out-of-memory on small CI runners.
#   COUNT     (optional) cap on how many source files to consider (sorted, deterministic).
#             Default 0 = no cap (the whole subtree). Pass e.g. 500 for a quick local run.
#
# Per-target tool selection (see jals-tests/README.md for where to get each):
#   gjf        GJF_JAR      a google-java-format "all-deps" jar (needs Java 21+)
#   palantir   PJF_BIN      a palantir-java-format native-image binary (no JVM needed)
#   eclipse    ECLIPSE_CP   a classpath holding the Eclipse JDT formatter jars; the tiny
#                           driver in scripts/eclipse/ is compiled against it
#   intellij   IDEA_HOME    an unpacked IntelliJ IDEA installation (uses bin/format.sh)
#
# The formatter's own config, where it has one, is read from `jals-tests/config/` — the
# same file `jals_tests::golden` imports to build the scoring config, so the corpus and
# the score can never be produced by two different styles.
#
# Files the formatter refuses to format (parse errors, unsupported syntax) are detected
# as "scratch copy came back byte-identical" and skipped, so the corpus only ever
# contains pairs the tool actually produced. A file that was *already* in the target
# style is skipped by the same rule; that only ever drops a would-be 100% match, it never
# admits a wrong pair.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # jals-tests/
target="${1:-}"
count="${2:-0}"
src_root="$here/sources/openjdk"
subtree="${SUBTREE:-}"
jobs="${JOBS:-2}"

case "$target" in
gjf | palantir | eclipse | intellij) ;;
*)
  echo "usage: $(basename "$0") <gjf|palantir|eclipse|intellij> [COUNT]" >&2
  exit 2
  ;;
esac

out_root="$here/sources/openjdk-$target"

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

# JDK 16+ closed off javac internals; the javac-backed formatters need them re-exported.
addexports=(
  --add-exports jdk.compiler/com.sun.tools.javac.api=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.file=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.main=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.parser=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.tree=ALL-UNNAMED
  --add-exports jdk.compiler/com.sun.tools.javac.util=ALL-UNNAMED
)

# A tool handed to us through the environment, so a bad value is the caller's to fix.
require_tool() {
  local var="$1" path="$2" what="$3"
  if [[ -z "$path" || ! -f "$path" ]]; then
    echo "error: set $var to $what (got: '${path:-unset}')" >&2
    exit 2
  fi
}

# A config file that ships in this repository, so a missing one is a broken checkout.
require_committed() {
  if [[ ! -f "$1" ]]; then
    echo "error: missing repository file $1" >&2
    exit 2
  fi
}

# --- Per-target seam -------------------------------------------------------------------
# `check_tool` fails fast on a tool that cannot run at all, and `format_scratch` formats
# every .java under $scratch in place. Everything else in this script is target-neutral.

case "$target" in
gjf)
  jar="${GJF_JAR:-}"
  require_tool GJF_JAR "$jar" "a google-java-format all-deps jar"
  check_tool() { java "${addexports[@]}" -jar "$jar" --version >/dev/null 2>&1; }
  format_scratch() {
    # Batched, warm JVMs: xargs amortizes JVM startup across ~hundreds of files per
    # process. google-java-format exits 1 when it skips a file it cannot parse, so the
    # non-zero exit is tolerated; failures are detected by the unchanged scratch copy.
    find "$scratch" -name '*.java' -print0 \
      | xargs -0 -P "$jobs" -n 200 java "${addexports[@]}" -jar "$jar" --replace || true
  }
  ;;
palantir)
  bin="${PJF_BIN:-}"
  require_tool PJF_BIN "$bin" "a palantir-java-format native-image binary"
  check_tool() { "$bin" --version >/dev/null 2>&1; }
  format_scratch() {
    # `--palantir` is required: the CLI's *default* is Google style, and the corpus has to
    # match `Target::palantir_config` (block 4 / continuation 8 / 120 columns).
    find "$scratch" -name '*.java' -print0 \
      | xargs -0 -P "$jobs" -n 200 "$bin" --palantir --replace || true
  }
  ;;
eclipse)
  classpath="${ECLIPSE_CP:-}"
  if [[ -z "$classpath" ]]; then
    echo "error: set ECLIPSE_CP to the Eclipse JDT formatter classpath" >&2
    echo "       run: jals-tests/scripts/fetch-eclipse-jdt.sh" >&2
    exit 2
  fi
  prefs="$here/config/eclipse-jals.prefs"
  require_committed "$prefs"
  driver_src="$here/scripts/eclipse/EclipseFormat.java"
  driver_out="$here/vendor/eclipse-driver"
  check_tool() {
    mkdir -p "$driver_out"
    javac -nowarn -cp "$classpath" -d "$driver_out" "$driver_src" >&2
  }
  format_scratch() {
    # One JVM formats the whole batch: the driver takes the profile once and walks the
    # file list, so there is no per-file startup to amortize.
    find "$scratch" -name '*.java' -print0 \
      | xargs -0 -P "$jobs" -n 400 \
        java -cp "$classpath:$driver_out" EclipseFormat "$prefs" || true
  }
  ;;
intellij)
  idea="${IDEA_HOME:-}"
  if [[ -z "$idea" || ! -x "$idea/bin/format.sh" ]]; then
    echo "error: set IDEA_HOME to an unpacked IntelliJ IDEA (got: '${idea:-unset}')" >&2
    exit 2
  fi
  scheme="$here/config/intellij-jals.xml"
  require_committed "$scheme"
  check_tool() { [[ -x "$idea/bin/format.sh" ]]; }
  format_scratch() {
    # IDEA's formatter is directory-recursive and starts a whole IDE per run, so it is
    # called exactly once — JOBS does not apply.
    #
    # `format.sh` forwards its argv to the formatter application, so JVM settings cannot be
    # passed as arguments; `_JAVA_OPTIONS` is the channel that reaches the JVM. Headless
    # mode keeps it from reaching for a display, and the throwaway config/system paths are
    # not hygiene but a requirement: IDEA refuses to start when another instance holds the
    # default ones.
    local ide_home="$scratch.idea-config"
    mkdir -p "$ide_home"
    _JAVA_OPTIONS="-Djava.awt.headless=true -Didea.config.path=$ide_home/config -Didea.system.path=$ide_home/system -Didea.plugins.path=$ide_home/plugins -Didea.log.path=$ide_home/log" \
      "$idea/bin/format.sh" -s "$scheme" -r -m '*.java' "$scratch" >&2 || true
    rm -rf "$ide_home"
  }
  ;;
esac

# Fail fast on a corrupt/missing tool: the batch below runs under `|| true` (formatters
# exit non-zero when they skip an unsupported file), which would otherwise mask a tool
# that cannot run at all.
if ! check_tool; then
  echo "error: the $target formatter failed to run — see the diagnostics above" >&2
  exit 2
fi

# Scratch holds throwaway copies we format in place; out_tmp accumulates the final pairs
# and is swapped into place only on success, so a crash never leaves a partial corpus.
# Both live next to out_root (same filesystem) so the final `mv` is an atomic rename, and
# the `openjdk-*.tmp.*` names are gitignored so a crash never litters the worktree.
scratch="$(mktemp -d "$here/sources/openjdk-$target.tmp.scratch.XXXXXX")"
out_tmp="$(mktemp -d "$here/sources/openjdk-$target.tmp.out.XXXXXX")"
trap 'rm -rf "$scratch" "$out_tmp" "$scratch.idea-config"' EXIT

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

echo "formatting ${#selected[@]} files with $target (jobs=$jobs) ..." >&2
format_scratch

ok=0
skipped=0
for rel in "${selected[@]}"; do
  orig="$src_root/$rel"
  formatted="$scratch/$rel"
  # The formatter left the scratch copy byte-identical → it failed, declined, or the file
  # was already in the target style. Either way there is nothing to learn from the pair.
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

echo "done: $ok pairs generated, $skipped skipped ($target declined) -> $out_root" >&2
