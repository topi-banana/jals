// Select the Minecraft distribution from `[features]`: `server` (the default), `client`, or —
// with both enabled — the merged jar. The resolved feature set is always part of the build-script
// fingerprint, so changing `--features` re-runs this script without any `rerun_if_env_changed`.
let server = build.feature("server");
let client = build.feature("client");
if !server && !client {
    // `--no-default-features` with no side selected: fall back to the server distribution.
    server = true;
}

// Minecraft 1.21.1 version metadata (immutable, content-addressed by Mojang).
let meta = tasks.fetch_json(
    tasks.https_url("https://piston-meta.mojang.com/v1/packages/d2ce7afeae88d45f482bc64681efafcb91ed4cca/1.21.1.json"),
    tasks.sha1("d2ce7afeae88d45f482bc64681efafcb91ed4cca"),
    tasks.bytes(65536)
);

let remapped = ();
let bundler = ();
if server {
    // The official server.jar is a bundler: the obfuscated game lives at
    // META-INF/versions/1.21.1/server-1.21.1.jar and libraries under META-INF/libraries/.
    bundler = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "server", "url"]),
        tasks.json_sha1(meta, ["downloads", "server", "sha1"]),
        tasks.json_u64(meta, ["downloads", "server", "size"])
    );
    let server_jar = tasks.nested_jar(bundler, "META-INF/versions/1.21.1/server-1.21.1.jar");
    let server_map = tasks.fetch_text(
        tasks.json_url(meta, ["downloads", "server_mappings", "url"]),
        tasks.json_sha1(meta, ["downloads", "server_mappings", "sha1"]),
        tasks.json_u64(meta, ["downloads", "server_mappings", "size"])
    );
    remapped = tasks.remap_jar(server_jar, server_map);
    // Flatten nested library jars (brigadier, guava, netty, …) onto the compile classpath. A
    // client-only build has no bundler, so those libraries are absent from its classpath.
    tasks.add_nested_classpath(bundler);
}
if client {
    let client_jar = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "client", "url"]),
        tasks.json_sha1(meta, ["downloads", "client", "sha1"]),
        tasks.json_u64(meta, ["downloads", "client", "size"])
    );
    let client_map = tasks.fetch_text(
        tasks.json_url(meta, ["downloads", "client_mappings", "url"]),
        tasks.json_sha1(meta, ["downloads", "client_mappings", "sha1"]),
        tasks.json_u64(meta, ["downloads", "client_mappings", "size"])
    );
    let client_remapped = tasks.remap_jar(client_jar, client_map);
    if server {
        // Both sides selected: overlay the client onto the server (client wins path conflicts).
        remapped = tasks.merge_jars(remapped, client_remapped);
    } else {
        remapped = client_remapped;
    }
}

// Remapped game bytecode on the classpath (libraries from the server bundler above).
tasks.add_classpath(remapped);
let sources = tasks.decompile_java(remapped, "net/minecraft");
tasks.publish_tree(
    "minecraft-1.21.1",
    sources,
    "src/main/java/net/minecraft",
    "replace-root"
);
