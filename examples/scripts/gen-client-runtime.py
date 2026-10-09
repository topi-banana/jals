#!/usr/bin/env python3
"""Write the runtime library table of `examples/minecraft_client_test/runtime.tsv`.

A build script cannot walk a release's `libraries` array: `Tasks.fetchJson` returns a *task handle*
and the fetch happens after the script has finished, so `Tasks.jsonAt` projects one named field and
there is no index, no length and no loop. The list of jars a client needs therefore has to be
written out, and the harness supports every release the SDK does — 44 of them, ~2400 jars in total,
each with its SHA-1 and its byte count. Nobody would keep that up by hand. This does it, and writes
it as data: `build.java` reads `runtime.tsv` at script run time and parses the fields, so a
regeneration is a diff of the file whose content actually moved rather than of a Java source with a
table spliced into it.

Four judgements live here rather than in the manifest:

* **Which releases.** The catalog is `examples/minecraft/build.java`'s `CATALOG`, read out of that
  file rather than restated here. It is already the one list of releases this repository agrees on,
  and it carries the SHA-1 that makes each metadata fetch content-addressed — so no mutable version
  manifest is consulted, exactly as on the SDK side.
* **Platform.** A library entry carries `rules` that admit it per operating system; this evaluates
  them for linux/x86_64 and nothing else. The example says so, and says why: the boot needs a GL
  stack, and CI has one on linux only.
* **Natives.** The native classifier jars go on the *classpath* rather than being unpacked to a
  directory. LWJGL's own `SharedLibraryLoader` extracts them from there, so `java.library.path`
  never has to be set and jals needs no notion of a runtime directory. Measured, not assumed — the
  client boots with no `-Djava.library.path` at all.
* **Byte budget.** `fetchJar` takes the size the release metadata states. The digest is what makes
  the bytes right; the count is what stops a redirect to something enormous.

Two metadata shapes carry those natives, and both are read:

* 1.19 and later put every artifact under `downloads.artifact`, one library entry per classifier,
  with the architecture in the Maven coordinate.
* 1.14.4 through 1.18.2 put the plain jar under `downloads.artifact` and its natives under
  `downloads.classifiers[<classifier>]`, named by a sibling `natives` map keyed on the launcher's
  OS name. Reading only the first shape there silently drops LWJGL's natives and the boot dies in
  `SharedLibraryLoader` with its cause two files away.

Usage:

    python3 examples/scripts/gen-client-runtime.py

It takes no arguments and always writes **every** release in the catalog. A per-release mode would
have to splice one release's rows into a committed table of 44, and a table that is partly
regenerated is exactly the failure this file's gap check exists to prevent. Re-run it when the
catalog gains a release. The result is committed — this is not a build step, and CI never runs it.
"""

from __future__ import annotations

import hashlib
import json
import os
import pathlib
import re
import stat
import sys
import tempfile
import urllib.request

# Every artifact a client links against is published here, so the table stores the path and this
# prefix is written once. A metadata entry that names another host is a gap rather than a longer
# row: it would mean Mojang had changed where a client's libraries come from, which is a thing to
# look at rather than to encode.
LIBRARIES = "https://libraries.minecraft.net/"
METADATA = "https://piston-meta.mojang.com/v1/packages/{sha1}/{version}.json"
# Every architecture token a Maven classifier's last segment may be, and the subset this runtime
# admits. A denylist would let the next one through; naming both halves means an unrecognised token
# is treated as "not an architecture" and the entry survives, which is the same reading a classifier
# like `natives-linux` gets.
ARCHITECTURES = frozenset(
    {
        "aarch_64",
        "aarch64",
        "amd64",
        "arm32",
        "arm64",
        "loongarch64",
        "ppc64le",
        "riscv64",
        "s390x",
        "x64",
        "x86",
        "x86_64",
    }
)
X86_64 = frozenset({"amd64", "x64", "x86_64"})
# The `natives` map is keyed on the launcher's own OS names, and its values may carry `${arch}`.
# Both halves are this platform's, in one place.
NATIVES_OS = "linux"
NATIVES_ARCH = "64"
# Seconds any one metadata fetch may take. Generous, because a release's metadata is one small
# document over a CDN, and the point is a run that ends rather than one that is quick.
HTTP_TIMEOUT = 60
EXAMPLES = pathlib.Path(__file__).resolve().parents[1]
# The table this generator owns. It is data, not source: the script that reads it parses four
# tab-separated fields per line, so a release's rows can move without any Java being rewritten.
RUNTIME = EXAMPLES / "minecraft_client_test" / "runtime.tsv"
# The catalog it reads releases from, and the Java declaration those rows live in.
CATALOG = EXAMPLES / "minecraft" / "build.java"
# `{ "<version>", "<40 hex>", "<bool>", "<bool>" }` — the SDK catalog's row shape. Only the first
# two fields matter here; the bundler and obfuscation flags describe the *game* jar, which the SDK
# fetches and this table never touches. Deliberately read without the trailing fields, so the Java
# may spell the flags however a reader likes.
CATALOG_ROW = re.compile(r'\{\s*"([^"]+)"\s*,\s*"([0-9a-f]{40})"')
# The shape a digest has to have before it is written into the table, matching the one the catalog
# row above is read back through.
SHA1 = re.compile(r"[0-9a-f]{40}")
# The declaration the rows are read out of. Scanned for between these two, rather than over the
# whole file: the row pattern is generic enough to match a commented-out example, a doc comment
# showing the shape, or a second table added later, and every spurious match becomes a release this
# script fetches metadata for and writes a row for. A table that grew a release nobody declared is
# one `build.java` loops over and no CI cell covers.
CATALOG_OPEN = "String[][] CATALOG = {"
CATALOG_CLOSE = re.compile(r"\n\s*\};")


class Catalog:
    """The releases to write, read out of the SDK example's own `CATALOG`."""

    @staticmethod
    def read(source: pathlib.Path) -> list[tuple[str, str]]:
        text = source.read_text(encoding="utf-8")
        start = text.find(CATALOG_OPEN)
        if start < 0:
            raise SystemExit(f"{source} declares no `{CATALOG_OPEN}`")
        close = CATALOG_CLOSE.search(text, start)
        if close is None:
            raise SystemExit(f"{source}'s `{CATALOG_OPEN}` is never closed")
        rows = CATALOG_ROW.findall(text[start + len(CATALOG_OPEN) : close.start()])
        if not rows:
            raise SystemExit(f"{source} declares no `CATALOG` rows this script recognises")
        return rows


class Runtime:
    """The libraries one release needs on linux/x86_64."""

    def __init__(self, release: str, meta_sha1: str) -> None:
        self.release = release
        self.meta = self.get_json(METADATA.format(sha1=meta_sha1, version=release), meta_sha1)
        # Walked once. `render` and the summary both want the result, and the scan reports what it
        # could not read — reporting it twice would read as two distinct gaps in the metadata.
        self.libraries, self.gaps = self.scan()

    @staticmethod
    def get_json(url: str, sha1: str) -> dict:
        # With a deadline. Forty-four sequential fetches and no timeout is a run that can stall
        # on one connection forever, and this script writes nothing until every release has been
        # read — so a stall produces no output and no account of where it stopped.
        with urllib.request.urlopen(url, timeout=HTTP_TIMEOUT) as response:
            body = response.read()
        # Checked, not merely addressed. The catalog carries the digest so the fetch names one
        # immutable document, and every one of the 2352 rows written below is derived from these
        # bytes — so a proxy, a CDN edge or a poisoned resolver that answers with something else
        # would be pinned into a committed file that nothing downstream can tell from the real one.
        # The SDK side verifies the same document through `Tasks.sha1`; this is that check.
        digest = hashlib.sha1(body).hexdigest()
        if digest != sha1:
            raise SystemExit(f"{url} answered with sha1 {digest}, not the catalog's {sha1}")
        return json.loads(body)

    @staticmethod
    def admits_x86_64(name: str) -> bool:
        """Reject a coordinate whose classifier names an architecture other than x86_64.

        A library entry's `rules` describe the *operating system*; the architecture of a native
        artifact is in the Maven classifier (`linux-aarch_64` beside `linux-x86_64`), and Mojang
        admits both through one linux rule. Reading only the rules therefore puts an aarch64 native
        on an x86_64 classpath. A classifier that names no architecture at all (`natives-linux`) is
        admitted unchanged.
        """
        parts = name.split(":")
        if len(parts) < 4:
            return True
        architecture = parts[3].rsplit("-", 1)[-1]
        if architecture in ARCHITECTURES:
            return architecture in X86_64
        return True

    @staticmethod
    def admits_linux(rules: list | None) -> bool:
        """Evaluate a library's `rules` for linux/x86_64.

        Last matching rule wins, as the launcher reads them. A rule keyed on `features` is a
        launcher toggle (demo mode, a custom resolution) that this runtime never sets, so it never
        matches.
        """
        if not rules:
            return True
        verdict = False
        for rule in rules:
            if "features" in rule:
                continue
            operating_system = rule.get("os", {})
            if operating_system.get("name", "linux") != "linux":
                continue
            if operating_system.get("arch", "x86_64") not in ("x86_64", "x64"):
                continue
            verdict = rule["action"] == "allow"
        return verdict

    def admit(self, name: str, download: dict, admitted: list, gaps: list) -> None:
        """Turn one `downloads.*` object into a table row, or into a gap.

        Repeats are dropped. Older metadata lists a library once plainly and again under an OS rule
        for its natives, and both carry the same `downloads.artifact` — so the same jar would go on
        the classpath twice. First occurrence wins, because classpath order is what decides which
        copy of a duplicated class is loaded and the first is the one that already decided it. Two
        entries claiming the same path with *different* bytes is a different thing entirely and is
        reported rather than silently resolved.
        """
        # `.get`, not `[...]`: a metadata object missing one of the three keys is the same kind of
        # thing as one carrying a digest of the wrong shape, and the caller refuses to rewrite the
        # committed table over either. Indexing instead raised a bare `KeyError` naming neither the
        # release nor the library, after forty-odd fetches — the cause-two-files-away failure this
        # whole gap mechanism exists to prevent.
        url = download.get("url")
        if not isinstance(url, str):
            gaps.append(f"{name}: its metadata states no download URL")
            return
        if not url.startswith(LIBRARIES):
            gaps.append(f"{name}: {url} is not published under {LIBRARIES}")
            return
        path = url[len(LIBRARIES) :]
        # Checked before anything is remembered about it, because all three go into a
        # tab-separated row verbatim — the catalog's own row is read back through a `[0-9a-f]{40}`
        # and this table deserves the same. A tab or a newline in a path writes a `runtime.tsv`
        # whose fields do not line up; a digest or a size of another shape writes one that parses
        # and pins something the metadata did not say.
        if "\t" in path or "\n" in path or "\r" in path:
            gaps.append(f"{name}: {path} is not spellable as a tab-separated field")
            return
        sha1 = download.get("sha1")
        if not SHA1.fullmatch(str(sha1)):
            gaps.append(f"{name}: {path} has no SHA-1 digest in its metadata")
            return
        size = download.get("size")
        if not isinstance(size, int) or isinstance(size, bool):
            gaps.append(f"{name}: {path} has no integer size in its metadata")
            return
        seen = self.seen.get(path)
        if seen is not None:
            if seen != sha1:
                gaps.append(f"{name}: {path} is listed twice with different digests")
            return
        self.seen[path] = sha1
        admitted.append((name, path, sha1, size))

    def scan(self) -> tuple[list[tuple[str, str, str, int]], list[str]]:
        """`(name, path, sha1, size)` for every admitted library in metadata order, and the gaps.

        A gap is a library this generator admitted but could not write out. It is returned rather
        than printed so the caller can refuse to rewrite a committed file over one: a table silently
        missing its LWJGL natives is a boot that dies in `SharedLibraryLoader` with its cause two
        files away.
        """
        admitted: list[tuple[str, str, str, int]] = []
        gaps: list[str] = []
        self.seen: dict[str, str] = {}
        for library in self.meta["libraries"]:
            if not self.admits_linux(library.get("rules")):
                continue
            name = library["name"]
            if not self.admits_x86_64(name):
                continue
            downloads = library.get("downloads", {})
            artifact = downloads.get("artifact")
            if artifact is not None:
                self.admit(name, artifact, admitted, gaps)
            # The pre-1.19 shape: a `natives` map names the classifier for this OS, and the jar
            # itself sits under `downloads.classifiers`. It is *beside* the ordinary artifact
            # rather than instead of it, so both are taken and the absent-artifact test below only
            # fires for an entry that has neither.
            classifier = library.get("natives", {}).get(NATIVES_OS)
            if classifier is not None:
                classifier = classifier.replace("${arch}", NATIVES_ARCH)
                native = downloads.get("classifiers", {}).get(classifier)
                if native is None:
                    gaps.append(
                        f"{name}: `natives.{NATIVES_OS}` names {classifier!r}, which"
                        " `downloads.classifiers` does not carry"
                    )
                else:
                    self.admit(f"{name}:{classifier}", native, admitted, gaps)
            elif artifact is None:
                gaps.append(f"{name}: no `downloads.artifact` and no `natives.{NATIVES_OS}`")
        return admitted, gaps

    def render(self) -> list[str]:
        """This release's block: a `[release]` header, then `path<TAB>sha1<TAB>bytes` rows."""
        lines = [f"[{self.release}]"]
        lines.extend(
            f"{path}\t{sha1}\t{size}" for _name, path, sha1, size in self.libraries
        )
        return lines


class Table:
    """Every release's libraries, as the one `runtime.tsv` the build script parses."""

    def __init__(self, catalog: list[tuple[str, str]]) -> None:
        self.runtimes = [Runtime(release, sha1) for release, sha1 in catalog]

    @property
    def gaps(self) -> list[tuple[str, str]]:
        return [(runtime.release, gap) for runtime in self.runtimes for gap in runtime.gaps]

    def render(self) -> str:
        """The whole file: a comment header, then every release's rows, newest first."""
        lines = [
            "# Every release's client runtime libraries, linux/x86_64, newest first — the ~60 jars",
            "# a client loads at boot that the SDK's game jar does not carry. A release opens with",
            "# `[<release>]`, and each row under it is",
            "# `<path under libraries.minecraft.net>\\t<sha1>\\t<bytes>`; the native classifier jars",
            "# are on the classpath like every other one, because LWJGL extracts what it needs out of",
            "# them itself, so no `java.library.path` and no unpacked directory are involved.",
            "#",
            "# Generated by examples/scripts/gen-client-runtime.py — do not edit by hand.",
        ]
        for runtime in self.runtimes:
            lines.extend(runtime.render())
        return "\n".join(lines)

    def write(self, destination: pathlib.Path) -> None:
        # Written through a sibling temporary file and renamed: the destination is committed, and a
        # process killed halfway through a plain overwrite leaves a truncated table that the build
        # script would read as a shorter list of libraries.
        body = self.render() + "\n"
        handle, temporary = tempfile.mkstemp(dir=str(destination.parent), suffix=".tsv")
        try:
            with os.fdopen(handle, "w", encoding="utf-8", newline="") as sink:
                sink.write(body)
            # `mkstemp` creates at 0600. The destination is a committed, world-readable data file,
            # and a rename would otherwise quietly narrow it.
            mode = destination.stat().st_mode if destination.exists() else 0o644
            os.chmod(temporary, stat.S_IMODE(mode))
            os.replace(temporary, destination)
        except BaseException:
            pathlib.Path(temporary).unlink(missing_ok=True)
            raise


class Main:
    @staticmethod
    def run(argv: list[str]) -> int:
        if len(argv) > 1:
            raise SystemExit(f"usage: {pathlib.Path(argv[0]).name}  (no arguments; writes every release)")
        table = Table(Catalog.read(CATALOG))
        if table.gaps:
            # Before the write, not after it. The output is committed, so writing a table this
            # generator knows to be incomplete would put the gap in the repository and leave the
            # only account of it in a terminal nobody kept. One gap in one release stops the whole
            # write, because the alternative is a table whose rows were generated at different
            # times against different metadata.
            for release, gap in table.gaps:
                print(f"cannot write {release}: {gap}", file=sys.stderr)
            raise SystemExit(f"{RUNTIME} left untouched")
        table.write(RUNTIME)
        rows = sum(len(runtime.libraries) for runtime in table.runtimes)
        unique = {library[1] for runtime in table.runtimes for library in runtime.libraries}
        print(
            f"wrote {rows} rows ({len(unique)} distinct jars)"
            f" for {len(table.runtimes)} releases into {RUNTIME}"
        )
        return 0


if __name__ == "__main__":
    sys.exit(Main.run(sys.argv))
