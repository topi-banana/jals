package demo;

// Provenance for FakeOrdinal.class — a non-enum class that happens to declare its own `ordinal()`.
// The switch subject reader recognises `invokevirtual …ordinal()I` as the enum lowering, so this is
// the shape that must still read back as a plain `int` switch instead of declining the method.
// Compiled with `javac` (JDK 25):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/FakeOrdinal.java
//     cp out/demo/FakeOrdinal.class jals-classpath/tests/fixtures/
public class FakeOrdinal {
    public int ordinal() {
        return 3;
    }

    public int onFake(FakeOrdinal f) {
        switch (f.ordinal()) {
            case 1:
                return 1;
            case 2:
                return 2;
            default:
                return 0;
        }
    }
}
