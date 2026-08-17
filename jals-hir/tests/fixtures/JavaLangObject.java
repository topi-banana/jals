// Provenance for java/lang/Object.class — a *classpath-origin* `java.lang.Object`, used by
// tests/type_mismatch.rs to check assignment against an `Object` the index holds as
// `ItemOrigin::Classpath` rather than as an embedded stub. The distinction is the whole point: a
// stub `Object` is demoted to its lenient by-name form by `Ty::demote_stdlib`, so it can never
// exercise the project-to-project subtyping arm, and a real JDK on the classpath does.
//
// `java.lang` belongs to `java.base`, so this needs `--patch-module` to compile at all. With
// `javac` (JDK 25), from the repository root:
//
//     mkdir -p /tmp/jl/java/lang
//     cp jals-hir/tests/fixtures/JavaLangObject.java /tmp/jl/java/lang/Object.java
//     javac --patch-module java.base=/tmp/jl -d /tmp/jlout /tmp/jl/java/lang/Object.java
//     cp /tmp/jlout/java/lang/Object.class jals-hir/tests/fixtures/JavaLangObject.class
//
// The member set is deliberately the one the *stub* `Object` declares (jals-hir/src/stdlib.rs), so
// the two indexes differ in origin and in nothing else.
package java.lang;

public class Object {
    public Object() {}

    public String toString() {
        return null;
    }

    public boolean equals(Object o) {
        return false;
    }

    public int hashCode() {
        return 0;
    }
}
