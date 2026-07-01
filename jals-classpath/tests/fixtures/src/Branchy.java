package demo;

// Provenance for Branchy.class — exercises the M2 if / if-else structuring: a guard-clause return,
// an if-else, a null-guarded field store, and a chained if / if. Compiled with `javac` (JDK 25):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Branchy.java
//     cp out/demo/Branchy.class jals-classpath/tests/fixtures/
public class Branchy {
    private int value;

    public int max(int a, int b) {
        if (a > b) {
            return a;
        }
        return b;
    }

    public String sign(int n) {
        if (n < 0) {
            return "neg";
        } else {
            return "pos";
        }
    }

    public void setIfNonNull(String s) {
        if (s != null) {
            this.value = s.length();
        }
    }

    public void classify(int n) {
        if (n < 0) {
            this.value = -1;
        } else {
            this.value = 1;
        }
        this.value = this.value + 1;
    }

    public int clamp(int x) {
        if (x < 0) {
            return 0;
        } else if (x > 100) {
            return 100;
        }
        return x;
    }
}
