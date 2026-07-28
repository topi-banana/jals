package demo;

// Provenance for Fors.class — `for`-loop recovery. A source `for` and the equivalent hand-written
// `while` compile to byte-identical code; only the LineNumberTable separates them, by putting a
// `for`'s update on the *header's* line. These methods cover both sides of that test plus the cases
// where the initializer's declaration may or may not be absorbed into the header. Compiled with -g
// so the LocalVariableTable can prove a counter dies with its loop:
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Fors.java
//     cp out/demo/Fors.class jals-classpath/tests/fixtures/
public class Fors {
    // The canonical `for`: the counter dies with the loop, so the header may declare it.
    public int sum(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            total = total + i;
        }
        return total;
    }

    // Two body statements share the latch block with the update, so only the last may be lifted.
    public int twoStmtBody(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            total = total + i;
            total = total + 1;
        }
        return total;
    }

    // A branch in the body: the latch is reached from both sides of the `if`.
    public int withIf(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            if (i > 0) {
                total = total + i;
            }
        }
        return total;
    }

    // An empty body and no init clause, with a counter that outlives the loop: the update still
    // folds, but the declaration stays hoisted.
    public int outlives(int n) {
        int i;
        i = 0;
        for (; i < n; i++) {
        }
        return i;
    }

    // A hand-written `while`, one statement per line — nothing shares the header's line, so it must
    // stay a `while`.
    public int whileLoop(int n) {
        int total = 0;
        int i = 0;
        while (i < n) {
            total = total + i;
            i = i + 1;
        }
        return total;
    }

    // The same `while` squeezed onto one line — initializer included — so every part of it shares
    // the header's line and the line test alone cannot tell it from a `for`. `i` outlives the loop,
    // so only the LocalVariableTable stops the declaration from being absorbed.
    public int inlineWhile(int n) {
        int total = 0;
        int i = 0; while (i < n) { total = total + i; i = i + 1; }
        return total + i;
    }

    // The counter is a *parameter*, so its declaration can never move into the header — doing so
    // would shadow the signature's own.
    public int reuse(int n) {
        int total = 0;
        for (n = 0; n < 10; n++) {
            total = total + n;
        }
        return total;
    }

    // Two updates in the header. Only the last statement of the latch can be lifted, so the other
    // update stays at the end of the body — a different shape from the source, but the same meaning.
    public int twoUpdates(int n) {
        int total = 0;
        for (int i = 0, j = n; i < j; i++, j--) {
            total = total + 1;
        }
        return total;
    }

    // Two sequential loops reusing one slot: they share a single hoisted declaration, so neither
    // may absorb it.
    public int twice(int n) {
        int total = 0;
        for (int i = 0; i < n; i++) {
            total = total + 1;
        }
        for (int i = 0; i < n; i++) {
            total = total + 1;
        }
        return total;
    }
}
