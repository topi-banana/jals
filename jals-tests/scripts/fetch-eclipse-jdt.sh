#!/usr/bin/env bash
# Fetch the Eclipse JDT jars the `openjdk-eclipse` golden corpus is generated with, and
# print the resulting classpath on stdout (progress goes to stderr):
#
#   ECLIPSE_CP="$(jals-tests/scripts/fetch-eclipse-jdt.sh)"
#
# The jars land in `jals-tests/vendor/eclipse-jdt/` (gitignored) and are only downloaded
# when missing, so a second run is a no-op.
#
# The coordinate list below is the *fully resolved* transitive closure of
# `org.eclipse.jdt:org.eclipse.jdt.core`, pinned. Resolving it here at run time would make
# the corpus depend on whatever a resolver picks that day, and the corpus is one half of a
# similarity metric that is only meaningful against a fixed reference (`DESIGN.md` §7.1).
# To move the pin, re-resolve and paste the result back in:
#
#   cs resolve org.eclipse.jdt:org.eclipse.jdt.core:<new-version> | sed 's/:default//'
#
# and update ECLIPSE_JDT_VERSION in `.github/workflows/ci.yml` and `TOOL_PINS` in
# `jals-tests/src/golden.rs` to match.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)" # jals-tests/
out="$here/vendor/eclipse-jdt"
base="${MAVEN_CENTRAL:-https://repo1.maven.org/maven2}"

# group:artifact:version, one per line. Keep sorted — it is a diffable pin, not a set.
coordinates=(
  net.java.dev.jna:jna:5.18.1
  net.java.dev.jna:jna-platform:5.18.1
  org.eclipse.jdt:ecj:3.46.0
  org.eclipse.jdt:org.eclipse.jdt.core:3.46.0
  org.eclipse.platform:org.eclipse.core.commands:3.12.500
  org.eclipse.platform:org.eclipse.core.contenttype:3.9.800
  org.eclipse.platform:org.eclipse.core.expressions:3.9.600
  org.eclipse.platform:org.eclipse.core.filesystem:1.11.400
  org.eclipse.platform:org.eclipse.core.jobs:3.15.700
  org.eclipse.platform:org.eclipse.core.resources:3.24.0
  org.eclipse.platform:org.eclipse.core.runtime:3.34.200
  org.eclipse.platform:org.eclipse.equinox.app:1.7.600
  org.eclipse.platform:org.eclipse.equinox.common:3.20.400
  org.eclipse.platform:org.eclipse.equinox.preferences:3.12.100
  org.eclipse.platform:org.eclipse.equinox.registry:3.12.600
  org.eclipse.platform:org.eclipse.osgi:3.24.200
  org.eclipse.platform:org.eclipse.text:3.14.700
  org.osgi:org.osgi.service.prefs:1.1.2
  org.osgi:osgi.annotation:8.0.1
)

mkdir -p "$out"
classpath=""
for coordinate in "${coordinates[@]}"; do
  IFS=: read -r group artifact version <<<"$coordinate"
  jar="$artifact-$version.jar"
  path="$out/$jar"
  if [[ ! -f "$path" ]]; then
    echo "fetching $coordinate ..." >&2
    # Download to a temporary name first: a half-written jar left by an interrupted run
    # would otherwise be taken for a complete one on the next.
    curl -fL --retry 3 -o "$path.part" \
      "$base/${group//.//}/$artifact/$version/$jar" >&2
    mv "$path.part" "$path"
  fi
  classpath="${classpath:+$classpath:}$path"
done

echo "$classpath"
