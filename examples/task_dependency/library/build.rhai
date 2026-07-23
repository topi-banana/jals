// A library whose classes are produced by a build task rather than checked in.
//
// Nothing here is aware of being a dependency: the same script is what this project would run as a
// root. What differs is where the results land — for a consumer they are projected into *its*
// verified cache, and this directory is never written to.

// `project_jar` keeps the example network-independent. A pinned remote archive substitutes
// directly, since every downstream handle is the same:
//
//     let jar = tasks.fetch_jar(
//         tasks.https_url("https://downloads.example.invalid/example.jar"),
//         tasks.sha256("<64 lowercase hexadecimal characters>"),
//         tasks.bytes(16777216)
//     );
let jar = tasks.project_jar("vendor/example.jar");

// Reaches the consumer's compile classpath and its analysis, exactly like a `jar` dependency.
tasks.add_classpath(jar);

if build.feature("sources") {
    // Reaches the consumer as read-only navigation sources, addressed `net/example/…` — the
    // destination's source root (`src/main/java`) is what gets stripped. They are never compile
    // inputs: the classpath jar above already defines these types.
    let sources = tasks.extract_java(jar, "net/example");
    tasks.publish_tree(
        "example-sources",
        sources,
        "src/main/java/net/example",
        "replace-root"
    );
}
