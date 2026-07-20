let jar = tasks.project_jar("vendor/example-sources.jar");
let sources = tasks.extract_java(jar, "net/example");
tasks.publish_tree(
    "example-sources",
    sources,
    "src/main/java/net/example",
    "replace-root"
);
