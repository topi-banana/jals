package demo;

// Provenance for Cmp.class — exercises the M7 numeric comparison conditions: a `lcmp`/`fcmpl`/
// `fcmpg`/`dcmpl`/`dcmpg` fused into the following `if<cond>` branch reads back as a
// long/float/double comparison, across all six operators, both NaN flavors, `if`/`while`/
// `do`-`while`, plus the two bails (a NaN-inexact rendering and a ternary's value merge).
// Compiled with `javac` (JDK 25):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Cmp.java
//     cp out/demo/Cmp.class jals-classpath/tests/fixtures/
public class Cmp {
    // lcmp + ifle (fall-through renders `>`).
    public long max(long a, long b) {
        if (a > b) {
            return a;
        }
        return b;
    }

    // fcmpg + ifge (fall-through renders `<`; NaN falls to the else side).
    public float floor(float f) {
        if (f < 0.0f) {
            return 0.0f;
        }
        return f;
    }

    // fcmpl + iflt (fall-through renders `>=`).
    public float atLeast(float f) {
        if (f >= 1.0f) {
            return f;
        }
        return 1.0f;
    }

    // dcmpg + ifgt (fall-through renders `<=`).
    public double cap(double d) {
        if (d <= 0.0) {
            return 0.0;
        }
        return d;
    }

    // dcmpl + ifne (fall-through renders `==` — exact under either flavor).
    public String same(double a, double b) {
        if (a == b) {
            return "eq";
        }
        return "ne";
    }

    // fcmpl + ifeq (fall-through renders `!=`).
    public String differ(float a, float b) {
        if (a != b) {
            return "ne";
        }
        return "eq";
    }

    // dcmpl + ifle exits a top-test while (fall-through renders `>`).
    public double halve(double d) {
        while (d > 1.0) {
            d = d / 2.0;
        }
        return d;
    }

    // fcmpg + iflt is a do-while back-edge (the taken side renders `<`).
    public float grow(float f) {
        do {
            f = f * 2.0f;
        } while (f < 100.0f);
        return f;
    }

    // `!(f < g)` is true on NaN — javac emits fcmpg + iflt, whose fall-through has no exact
    // single-operator rendering, so the NaN guard must bail the whole method.
    public float pickGuard(float f, float g) {
        if (!(f < g)) {
            return f;
        }
        return g;
    }

    // A ternary merges its value at the join with a leftover stack — still bails.
    public long least(long a, long b) {
        return a < b ? a : b;
    }
}
