package demo;

// Provenance for Outer.class / Outer$Inner.class / Outer$Color.class — a top-level class with a
// nested static class and a nested enum, used by tests/decompile.rs to check that the skeleton
// renderer groups nested types into their enclosing type's file (so dotted FQNs line up). Compiled
// with `javac` (JDK 25):
//     javac -d jals-classpath/tests/fixtures jals-classpath/tests/fixtures/Outer.java
public class Outer {
    public int field;

    public Outer(int x) {}

    public String greet(String name) {
        return name;
    }

    public static class Inner {
        public long value;

        public void run() {}
    }

    public enum Color {
        RED,
        GREEN,
        BLUE;

        public boolean isRed() {
            return this == RED;
        }
    }
}
