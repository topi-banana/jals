# Class-file test fixtures

These `.class` files are compiled locally with `javac` from the trivial Java sources in `src/`,
which were written for this crate (no third-party or GPL-licensed code). They drive the byte-exact
round-trip test in `../roundtrip.rs`.

The OpenJDK / google-java-format git submodules under `jals-tests/sources/` are **not** used as
fixtures: they are optional checkouts, not present in every CI job, and (for OpenJDK) GPL-licensed.

## Provenance / regenerating

Compiled with the JDK pinned for this repo (`javac 25`, class-file major version 69):

```sh
# The flat sources (Plain / Iface / Sample / TypeAnno / Switches):
javac -parameters -d out tests/fixtures/src/*.java
cp out/*.class tests/fixtures/

# The module (compiled separately so `module-info` gets a Module attribute):
javac -d modout tests/fixtures/src/module/module-info.java \
    tests/fixtures/src/module/com/example/demo/Api.java
cp modout/module-info.class tests/fixtures/
```

`-parameters` is passed so the `MethodParameters` attribute is emitted (extra attribute coverage).

## What each fixture exercises

| `.class` file | from | notable structures |
| --- | --- | --- |
| `Plain.class` | `Plain.java` | the minimal shape: a field, a method, `Code`, `SourceFile` |
| `Iface.class` | `Iface.java` | a generic interface, `Signature`, `default` methods (`Code`) |
| `Sample.class` | `Sample.java` | generics, `StackMapTable` (loop + branch), `InnerClasses`, `RuntimeVisibleAnnotations` (`@Deprecated`), `Deprecated`, `MethodParameters`, `ConstantValue` |
| `Sample$Kind.class` | `Sample.java` | an `enum` |
| `Sample$Visitor.class` | `Sample.java` | a nested generic interface |
| `Sample$Point.class` | `Sample.java` | a `record` (`Record` attribute, `NestHost`) |
| `Switches.class` | `Switches.java` | `tableswitch` (dense) + `lookupswitch` (sparse) — exercises switch alignment padding |
| `TypeAnno.class` | `TypeAnno.java` | `RuntimeVisibleTypeAnnotations` + `RuntimeInvisibleTypeAnnotations` (TYPE_USE) |
| `TypeAnno$RNonNull.class` / `TypeAnno$CNonNull.class` | `TypeAnno.java` | annotation types (`@interface`) |
| `module-info.class` | `module/module-info.java` | `Module` + `ModulePackages` attributes |
