// Select the Minecraft distribution: server (default), client, or merged.
let side = build.env("JALS_MC_SIDE");
if side == () {
    side = "server";
}
if side != "server" && side != "client" && side != "merged" {
    build.error("JALS_MC_SIDE must be server, client, or merged");
}
build.rerun_if_env_changed("JALS_MC_SIDE");

// Minecraft 1.21.1 version metadata (immutable, content-addressed by Mojang).
let meta = tasks.fetch_json(
    tasks.https_url("https://piston-meta.mojang.com/v1/packages/d2ce7afeae88d45f482bc64681efafcb91ed4cca/1.21.1.json"),
    tasks.sha1("d2ce7afeae88d45f482bc64681efafcb91ed4cca"),
    tasks.bytes(65536)
);

let remapped = ();
let bundler = ();
if side == "server" || side == "merged" {
    // The official server.jar is a bundler: the obfuscated game lives at
    // META-INF/versions/1.21.1/server-1.21.1.jar and libraries under META-INF/libraries/.
    bundler = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "server", "url"]),
        tasks.json_sha1(meta, ["downloads", "server", "sha1"]),
        tasks.json_u64(meta, ["downloads", "server", "size"])
    );
    let server = tasks.nested_jar(bundler, "META-INF/versions/1.21.1/server-1.21.1.jar");
    let server_map = tasks.fetch_text(
        tasks.json_url(meta, ["downloads", "server_mappings", "url"]),
        tasks.json_sha1(meta, ["downloads", "server_mappings", "sha1"]),
        tasks.json_u64(meta, ["downloads", "server_mappings", "size"])
    );
    remapped = tasks.remap_jar(server, server_map);
    // Flatten nested library jars (brigadier, guava, netty, …) onto the compile classpath.
    tasks.add_nested_classpath(bundler);
}
if side == "client" || side == "merged" {
    let client = tasks.fetch_jar(
        tasks.json_url(meta, ["downloads", "client", "url"]),
        tasks.json_sha1(meta, ["downloads", "client", "sha1"]),
        tasks.json_u64(meta, ["downloads", "client", "size"])
    );
    let client_map = tasks.fetch_text(
        tasks.json_url(meta, ["downloads", "client_mappings", "url"]),
        tasks.json_sha1(meta, ["downloads", "client_mappings", "sha1"]),
        tasks.json_u64(meta, ["downloads", "client_mappings", "size"])
    );
    let client_remapped = tasks.remap_jar(client, client_map);
    if side == "merged" {
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
