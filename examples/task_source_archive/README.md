# Typed source-archive task

This example reserves `src/main/java/net/example` for sources extracted from
`vendor/example-sources.jar`. Supply a JAR containing `net/example/Generated.java`, then run:

```sh
jals build
```

`tasks.project_jar` keeps this checked-in example network-independent. A pinned remote archive uses
the same downstream handles:

```rhai
let jar = tasks.fetch_jar(
    tasks.https_url("https://downloads.example.invalid/example-sources.jar"),
    tasks.sha256("<64 lowercase hexadecimal characters>"),
    tasks.bytes(16777216)
);
```

Static downloads require HTTPS, an expected SHA-1 or SHA-256 digest, and a non-zero byte limit.
Successful bytes are stored in the verified SHA-256 project cache; `jals build --offline` can reuse
them.

The `replace-root` terminal owns the complete destination. Every successful changed result removes
all existing descendants before writing the new non-empty tree, including manual edits and untracked
files in that directory. Failures publish nothing. Dropping the terminal or running `jals clean`
removes the owned directory.
