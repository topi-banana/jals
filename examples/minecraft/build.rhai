// Every Minecraft release this example can build, newest first, as
// `[version, metadata SHA-1, bundler?, obfuscated?]`. A metadata URL is content-addressed
// (`…/v1/packages/<sha1>/<version>.json`), so the one digest pins both the URL and the fetch and no
// mutable version manifest is ever consulted.
//
// The two flags are independent axes:
//
// - `bundler?` marks 1.18 and later, whose server download wraps the game jar at
//   `META-INF/versions/<version>/server-<version>.jar` with its libraries under
//   `META-INF/libraries/`; earlier releases ship one flat jar with the libraries alongside the game
//   classes.
// - `obfuscated?` marks the releases that need deobfuscating: the script fetches the official
//   mappings and runs `remap_jar` only for those. From 26.1 Mojang ships the game with its real
//   names already in the jar, and the version metadata carries no
//   `client_mappings`/`server_mappings` download at all — so the whole remap step (mappings fetch
//   included) is skipped there.
//
// The catalog is bounded at both ends by mappings: 1.14.4 is the oldest entry because nothing
// before it has official mappings, and 26.1 onward needs none.
const CATALOG = [
    ["26.2", "d98675ecc24364e90b18dbea80390b1345c3f71f", true, false],
    ["26.1.2", "8228875b88ad88b4bc0dc7a2afbc6903bccf93b1", true, false],
    ["26.1.1", "06e0e05c144ccf6ae8c5f766093463b20d221631", true, false],
    ["26.1", "330f499d3aa1d31fcc15e7ae97af5f93d8493ba6", true, false],
    ["1.21.11", "fb498c3c1beff5c406e71b776d8457499670cb3f", true, true],
    ["1.21.10", "53880e8f005452d97752315c801e854ca2e0672b", true, true],
    ["1.21.9", "26b3c0e988cf18a235de8a686d137f2d590cc7c8", true, true],
    ["1.21.8", "596fd9e5a2a01b2b02018e5d4292c9c5aaa23a78", true, true],
    ["1.21.7", "54e6650cb79f5fd4f786ce15d7549c40ba9c5d59", true, true],
    ["1.21.6", "a149ac8d831f00db3e847e411b66845714986d66", true, true],
    ["1.21.5", "e0ab7e984fc1d951e2264da9234f6554662154bb", true, true],
    ["1.21.4", "99af4119ace27af25fd44879e53fe25ea9eb8c9c", true, true],
    ["1.21.3", "fcd093df1bcc9a6f47e358ef84e9f6a7839c8805", true, true],
    ["1.21.2", "67e006ccdace75f49e4b45c953cd5beabc18e800", true, true],
    ["1.21.1", "67e466e82c012158c8cda81df39aa40a7ade7276", true, true],
    ["1.21", "46d5cd39c21f24186df2756fee3e4306582110d8", true, true],
    ["1.20.6", "959d1829dfc630d08a245294bcd78f7906e5d3d1", true, true],
    ["1.20.5", "9eafba9f2e7053964b35ad4ba82558c865b296f4", true, true],
    ["1.20.4", "99f86745e175a356b09e045479999631a345d16f", true, true],
    ["1.20.3", "f6b794c78b067649dde6899df14dce4cd15925b4", true, true],
    ["1.20.2", "75d1c6627fde6b7815fa11bfab54ea8accfe07f3", true, true],
    ["1.20.1", "8a4e093bfaa91de10c17af807570cdd64468bf67", true, true],
    ["1.20", "5f84c06e6ca08afba5991868efad15bb04b95160", true, true],
    ["1.19.4", "b2a4a55527f44f5b9f807675e795591ddfa5063a", true, true],
    ["1.19.3", "a0f299abf6a033258095c25f1d6555847b8fdb43", true, true],
    ["1.19.2", "ed548106acf3ac7e8205a6ee8fd2710facfa164f", true, true],
    ["1.19.1", "39d5e8925d37490c6f2abb2e02b8c6f1b35719df", true, true],
    ["1.19", "14bbfb25fb1c1c798e3c9b9482b081a78d1f3a9d", true, true],
    ["1.18.2", "334b33fcba3c9be4b7514624c965256535bd7eba", true, true],
    ["1.18.1", "7ff864e988a2c29907154d5f9701e87e5d5e554a", true, true],
    ["1.18", "7367ea8b7cad7c7830192441bb2846be0d2ceeac", true, true],
    ["1.17.1", "e0e7ab5ed6f55bbd874ef95be3c9356d67e64b57", false, true],
    ["1.17", "0d9ace8a2ecfd1f4c782786f4b985a499240ff12", false, true],
    ["1.16.5", "fba9f7833e858a1257d810d21a3a9e3c967f9077", false, true],
    ["1.16.4", "596ad61fda7612d9edf8881cf81869276bdb7f82", false, true],
    ["1.16.3", "6485dd131ef68c968041a9f6fd73094b027e42e1", false, true],
    ["1.16.2", "998d9ef5770d05c20d760dc16cf85151f35009f2", false, true],
    ["1.16.1", "54fa3af57d041d2771e66d390197b2c0288e697c", false, true],
    ["1.16", "e9d21d375f9c961f0e9731d4e463306d76e77c48", false, true],
    ["1.15.2", "e9d0adb8f642abe422909ede50f651b2b58a3573", false, true],
    ["1.15.1", "18c3063de87ae126b4e017121219ba802be0755b", false, true],
    ["1.15", "833322370ab320e77717097082effe1d124d48bd", false, true],
    ["1.14.4", "be146d5f66a3627ed0a87c234c4d8dde8ab35098", false, true],
];

// The release built when `[features]` selects none. `[features] default` deliberately carries only
// the side, so `--features 1.20.1` picks a version without `--no-default-features` first having to
// drop a conflicting default version.
const DEFAULT_VERSION = "26.2";

// Resolve the release. The version features are mutually exclusive, but `[features]` resolution is
// additive (Cargo's model), so this script is the only place the exclusivity can be enforced. A
// reported error publishes nothing and stops the build before any task fetch runs.
let release = ();
for entry in CATALOG {
    if entry[0] == DEFAULT_VERSION {
        release = entry;
    }
}
let selected = "";
for entry in CATALOG {
    if build.feature(entry[0]) {
        if selected != "" {
            build.error(
                "select at most one Minecraft version feature, got `" + selected + "` and `"
                    + entry[0] + "`"
            );
            return;
        }
        selected = entry[0];
        release = entry;
    }
}
let version = release[0];
let meta_sha1 = release[1];
let bundled = release[2];
let obfuscated = release[3];
build.metadata("minecraft-version", version);

// Select the Minecraft distribution from `[features]`: `server` (the default), `client`, or —
// with both enabled — the merged jar. The resolved feature set is always part of the build-script
// fingerprint, so changing `--features` re-runs this script without any `rerun_if_env_changed`.
let server = build.feature("server");
let client = build.feature("client");
if !server && !client {
    // `--no-default-features` with no side selected: fall back to the server distribution.
    server = true;
}

// The selected release's version metadata (immutable, content-addressed by Mojang). The largest
// metadata in the catalog is ~51 KiB.
let meta = tasks.fetch_json(
    tasks.https_url(
        "https://piston-meta.mojang.com/v1/packages/" + meta_sha1 + "/" + version + ".json"
    ),
    tasks.sha1(meta_sha1),
    tasks.bytes(65536)
);

let game = ();
if server {
    let download = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "server", "url"]),
        tasks.json_sha1(meta, ["downloads", "server", "sha1"]),
        tasks.json_u64(meta, ["downloads", "server", "size"])
    );
    let server_jar = download;
    if bundled {
        // 1.18+: the official server.jar is a bundler, so the game jar has to be pulled out of it.
        // Flatten its nested library jars (brigadier, guava, netty, …) onto the compile
        // classpath too; a client-only build has no bundler, so those libraries are absent from its
        // classpath. Older releases need neither call — the game classes and the libraries share
        // one flat jar, which `add_classpath` below already puts on the classpath.
        server_jar = tasks.nested_jar(
            download,
            "META-INF/versions/" + version + "/server-" + version + ".jar"
        );
        tasks.add_nested_classpath(download);
    }
    if obfuscated {
        // Up to 1.21.11 the shipped names are obfuscated, so the official mappings turn them back
        // into the real ones. 26.1+ ships them already deobfuscated and declares no mappings
        // download, so projecting `downloads.server_mappings` there would not even resolve.
        let server_map = tasks.fetch_text(
            tasks.json_url(meta, ["downloads", "server_mappings", "url"]),
            tasks.json_sha1(meta, ["downloads", "server_mappings", "sha1"]),
            tasks.json_u64(meta, ["downloads", "server_mappings", "size"])
        );
        server_jar = tasks.remap_jar(server_jar, server_map);
    }
    game = server_jar;
}
if client {
    let client_jar = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "client", "url"]),
        tasks.json_sha1(meta, ["downloads", "client", "sha1"]),
        tasks.json_u64(meta, ["downloads", "client", "size"])
    );
    if obfuscated {
        let client_map = tasks.fetch_text(
            tasks.json_url(meta, ["downloads", "client_mappings", "url"]),
            tasks.json_sha1(meta, ["downloads", "client_mappings", "sha1"]),
            tasks.json_u64(meta, ["downloads", "client_mappings", "size"])
        );
        client_jar = tasks.remap_jar(client_jar, client_map);
    }
    if server {
        // Both sides selected: overlay the client onto the server (client wins path conflicts).
        game = tasks.merge_jars(game, client_jar);
    } else {
        game = client_jar;
    }
}

// Deobfuscated game bytecode on the classpath (1.18+ libraries come from the server bundler above,
// older ones ride along inside this same jar).
tasks.add_classpath(game);
let sources = tasks.decompile_java(game, "net/minecraft");
tasks.publish_tree(
    "minecraft-" + version,
    sources,
    "src/main/java/net/minecraft",
    "replace-root"
);
