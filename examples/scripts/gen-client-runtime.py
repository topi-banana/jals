#!/usr/bin/env python3
"""Regenerate `examples/minecraft_client_e2e/build.rhai` for one Minecraft release.

    examples/scripts/gen-client-runtime.py 1.21.11

The vanilla launcher assembles a client from a release's metadata: the libraries it links against,
the native `.so` files the JVM needs on its library path, and the asset store its resources come
from. A build script declares the same thing, but declaratively and pinned by digest — so the list
has to be *derived* from the metadata rather than written by hand, and this is what derives it.

Three decisions are made here rather than in the script it writes, because each is a judgement about
the release and not about the build:

  * **Platform.** The `rules` on a library entry are evaluated for linux/x86_64, which is what CI
    runs. A cell on another platform needs its own generated script.
  * **Which assets.** Everything but the sounds, and only the English language file — 45 objects and
    about 16 MiB, against 4591 and 430 MiB for the whole store. The panorama the title screen renders
    is in the kept half; the sounds are what a screenshot never shows.
  * **Byte caps.** Rounded up to the next 64 KiB. The digest is what guarantees the content, so
    pinning an exact byte count would only ever break on a re-serve of identical bytes.
"""

import json
import sys
import urllib.request
from pathlib import Path

VERSION_MANIFEST = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json"
ASSET_BASE = "https://resources.download.minecraft.net"
OUT = Path(__file__).resolve().parents[1] / "minecraft_client_e2e" / "build.rhai"

# Where each native jar keeps its single `.so`. Stripping this prefix leaves a flat directory, which
# is what `-Djava.library.path` searches — it does not recurse.
def native_prefix(name: str) -> str:
    coord = name.split(":")[1]
    if coord == "jtracy":
        return ""
    if coord == "lwjgl":
        return "linux/x64/org/lwjgl"
    assert coord.startswith("lwjgl-"), f"unknown native library {name}"
    return "linux/x64/org/lwjgl/" + coord[len("lwjgl-") :]


def allowed(lib, os_name="linux", arch="x86_64") -> bool:
    rules = lib.get("rules")
    if not rules:
        return True
    ok = False
    for rule in rules:
        matches = True
        os_rule = rule.get("os")
        if os_rule:
            if "name" in os_rule and os_rule["name"] != os_name:
                matches = False
            if "arch" in os_rule and os_rule["arch"] != arch:
                matches = False
        if matches:
            ok = rule["action"] == "allow"
    return ok


def cap(size: int) -> int:
    step = 65536
    return max(step, ((size + step - 1) // step) * step)


def fetch_json(url: str):
    with urllib.request.urlopen(url) as response:
        return json.load(response)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    version = sys.argv[1]

    manifest = fetch_json(VERSION_MANIFEST)
    entry = next((v for v in manifest["versions"] if v["id"] == version), None)
    if entry is None:
        print(f"no such release: {version}", file=sys.stderr)
        return 1
    meta = fetch_json(entry["url"])
    index = fetch_json(meta["assetIndex"]["url"])["objects"]

    selected = [lib for lib in meta["libraries"] if allowed(lib)]
    plain = [lib for lib in selected if ":natives-" not in lib["name"]]
    natives = [lib for lib in selected if ":natives-" in lib["name"]]
    assets = {k: v for k, v in index.items() if "/sounds/" not in k}
    assets = {k: v for k, v in assets.items() if "/lang/" not in k or k.endswith("en_us.json")}
    assets = dict(sorted(assets.items()))

    out: list[str] = []
    w = out.append
    w(f"// Generated. Regenerate with `examples/scripts/gen-client-runtime.py {version}`.")
    w("//")
    w("// Everything the vanilla launcher would assemble, declared instead: the libraries the client")
    w("// links against, the native `.so` files the JVM needs on its library path, and the slice of the")
    w("// asset store a rendered frame actually reads. The game jar itself is not here — it arrives")
    w("// through `[dependencies] minecraft`, already remapped to official names, which is what lets the")
    w("// driver be written in Java rather than in reflection.")
    w("")
    w(f'if !build.feature("{version}") {{')
    w(f'    build.error("this example targets one release; run it with `--features {version}`.");')
    w("}")
    w("")
    w("// --- libraries -------------------------------------------------------------------------------")
    w(f"// The {len(plain)} entries the release metadata selects for linux/x86_64, minus the natives below.")
    w("")
    for lib in plain:
        a = lib["downloads"]["artifact"]
        w("tasks.add_classpath(tasks.fetch_jar(")
        w(f'    tasks.https_url("{a["url"]}"),')
        w(f'    tasks.sha1("{a["sha1"]}"), tasks.bytes({cap(a["size"])})));')
    w("")
    w("// --- natives ---------------------------------------------------------------------------------")
    w("// One `.so` per jar, each under its own prefix, stripped flat so `-Djava.library.path` finds")
    w("// them all in one directory. Merged rather than added one at a time because a runtime directory")
    w("// is one tree and the terminal takes one.")
    w("")
    first = True
    for lib in natives:
        a = lib["downloads"]["artifact"]
        expr = (
            "tasks.extract_files(tasks.fetch_jar(\n"
            f'        tasks.https_url("{a["url"]}"),\n'
            f'        tasks.sha1("{a["sha1"]}"), tasks.bytes({cap(a["size"])})), "{native_prefix(lib["name"])}")'
        )
        if first:
            w(f"let natives = {expr};")
            first = False
        else:
            w(f"natives = tasks.merge_trees(natives,\n    {expr});")
    w('tasks.add_runtime_dir("natives", natives);')
    w("")
    w("// --- assets ----------------------------------------------------------------------------------")
    total = (sum(v["size"] for v in assets.values()) + meta["assetIndex"]["size"]) / 1048576
    w(f"// {len(assets) + 1} objects, {total:.1f} MiB. The full store is {len(index)} objects and 430 MiB; the")
    w("// difference is the sounds and the other languages, neither of which a screenshot shows.")
    w("")
    ai = meta["assetIndex"]
    w(f'let assets = tasks.place("indexes/{ai["id"]}.json", tasks.fetch_bytes(')
    w(f'    tasks.https_url("{ai["url"]}"),')
    w(f'    tasks.sha1("{ai["sha1"]}"), tasks.bytes({cap(ai["size"])})));')
    for value in assets.values():
        h = value["hash"]
        w(f'assets = tasks.merge_trees(assets, tasks.place("objects/{h[:2]}/{h}", tasks.fetch_bytes(')
        w(f'    tasks.https_url("{ASSET_BASE}/{h[:2]}/{h}"),')
        w(f'    tasks.sha1("{h}"), tasks.bytes({cap(value["size"])}))));')
    w('tasks.add_runtime_dir("assets", assets);')
    w("")

    OUT.write_text("\n".join(out))
    print(f"wrote {OUT}")
    print(f"  libraries {len(plain)}  natives {len(natives)}  assets {len(assets) + 1}")
    print(f"  asset index id is {ai['id']} — `[[test-target]] args` names it, so update that too")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
