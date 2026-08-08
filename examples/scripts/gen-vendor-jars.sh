#!/usr/bin/env bash
# Generate the vendor JARs the two build-task examples read.
#
# `task_dependency` and `task_source_archive` both start from `tasks.project_jar(...)` — a JAR that
# lives in the project rather than behind a pinned URL, which is what keeps those two examples
# network-independent. A JAR is a binary, so neither is committed; this script is what supplies
# them, for a reader following the README and for the `examples` CI job alike.
#
#   examples/scripts/gen-vendor-jars.sh
#
# Both outputs are rewritten from scratch on every run. Needs `javac` and `jar` on PATH.
set -euo pipefail

examples="$(cd "$(dirname "$0")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p "$work/net/example"

# `task_dependency`: the library publishes this type twice — as the compiled class its
# `add_classpath` puts on the consumer's compile classpath, and (under the `sources` feature) as the
# `.java` its `extract_java` publishes as navigation sources. Both halves have to be in the one JAR.
cat >"$work/net/example/Greeter.java" <<'JAVA'
package net.example;

/** A library type that reaches a consumer through a build task rather than through source. */
public final class Greeter {
    private Greeter() {}

    /** The greeting the consumer's `Main` prints. */
    public static String greeting() {
        return "Hello from a build-task dependency!";
    }
}
JAVA

# `task_source_archive`: sources only. The archive is extracted and published into the project's own
# `src/main/java/net/example`, so this type is compiled from source there — no class file needed.
cat >"$work/net/example/Generated.java" <<'JAVA'
package net.example;

/** A type carried by a source archive and published into the project's own source tree. */
public final class Generated {
    /** What the archive contributes; `Main` declares a field of this type. */
    public static final String ORIGIN = "vendor/example-sources.jar";
}
JAVA

javac -d "$work" "$work/net/example/Greeter.java"

mkdir -p "$examples/task_dependency/library/vendor" "$examples/task_source_archive/vendor"

jar --create --file "$examples/task_dependency/library/vendor/example.jar" \
    -C "$work" net/example/Greeter.class \
    -C "$work" net/example/Greeter.java
jar --create --file "$examples/task_source_archive/vendor/example-sources.jar" \
    -C "$work" net/example/Generated.java

echo "wrote task_dependency/library/vendor/example.jar"
echo "wrote task_source_archive/vendor/example-sources.jar"
