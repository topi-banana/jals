// Provenance for java/lang/Integer.class — a *classpath-origin* `java.lang.Integer`, used by
// tests/type_mismatch.rs to check that autoboxing survives a wrapper the index holds as
// `ItemOrigin::Classpath`. A stub wrapper is demoted to its lenient by-name form by
// `Ty::demote_stdlib` and so only ever exercised the external arm of the boxing rule; a real JDK on
// the classpath reaches the project arm, which used to answer a flat `false`.
//
// The declared members are irrelevant — boxing is decided from the *name* (`Primitive::boxes_to`) —
// so this is deliberately the smallest class that carries the right fully-qualified name.
//
// `java.lang` belongs to `java.base`, so this needs `--patch-module` to compile at all. With
// `javac` (JDK 25), from the repository root:
//
//     mkdir -p /tmp/jl/java/lang
//     cp jals-hir/tests/fixtures/JavaLangObject.java  /tmp/jl/java/lang/Object.java
//     cp jals-hir/tests/fixtures/JavaLangInteger.java /tmp/jl/java/lang/Integer.java
//     javac --patch-module java.base=/tmp/jl -d /tmp/jlout /tmp/jl/java/lang/*.java
//     cp /tmp/jlout/java/lang/Object.class  jals-hir/tests/fixtures/JavaLangObject.class
//     cp /tmp/jlout/java/lang/Integer.class jals-hir/tests/fixtures/JavaLangInteger.class
package java.lang;

public class Integer {
    public Integer(int value) {}

    public int intValue() {
        return 0;
    }
}
