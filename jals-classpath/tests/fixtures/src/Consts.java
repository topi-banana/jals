package demo;

// Provenance for Consts.class — exercises the M0 skeleton enrichments: `ConstantValue` initializers
// across every constant kind, real parameter names (compiled with `-parameters -g`), a declared
// checked exception (`Exceptions` attribute), and the value-returning / void / constructor body
// shapes. Compiled with `javac` (JDK 25):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Consts.java
//     cp out/demo/Consts.class jals-classpath/tests/fixtures/
public class Consts {
    public static final int MAX = 42;
    public static final long BIG = 9000000000L;
    public static final double RATE = 1.5;
    public static final float RATIO = 0.25f;
    public static final boolean ENABLED = true;
    public static final String NAME = "jals";

    private int count;

    public Consts(int start) {
        this.count = start;
    }

    public int add(int delta) {
        return count + delta;
    }

    public void reset() {}

    public void risky(String path) throws java.io.IOException {
        throw new java.io.IOException(path);
    }
}
