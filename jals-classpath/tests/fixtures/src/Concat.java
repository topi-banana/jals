package demo;

/**
 * Fixture for M6 string concatenation: {@code invokedynamic makeConcatWithConstants} call sites
 * (javac's default lowering). Compile with {@code -parameters -g}; see ../README.md.
 */
class Concat {
    /** Chunks around one dynamic String operand: recipe {@code "Hello, !"}. */
    String greet(String name) {
        return "Hello, " + name + "!";
    }

    /** A leading chunk and a dynamic int operand: recipe {@code "n = "}. */
    String label(int n) {
        return "n = " + n;
    }

    /** Two dynamic operands, the first String-typed — no seed needed. */
    String pair(String a, int b) {
        return a + b;
    }

    /** The empty constant vanishes from the recipe; the fold must reintroduce the {@code ""}. */
    String bare(int a, int b) {
        return a + "" + b;
    }

    /** A constant containing a recipe marker char is passed via a bootstrap argument (the U+0002 recipe path). */
    String tagged(int n) {
        return "\u0001" + n;
    }

    /** A dynamic char operand (non-constant, so it stays out of the recipe). */
    String glue(String s, char c) {
        return s + c;
    }

    /** Mixed primitive operands with an inner chunk. */
    String mix(double d, boolean f) {
        return d + " & " + f;
    }

    /** A non-concat invokedynamic (LambdaMetafactory) — the method must fall back. */
    Runnable lazy() {
        return () -> {
        };
    }

    /** A discarded object creation must survive as an expression statement. */
    void ping() {
        new Concat();
    }
}
