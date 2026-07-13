package demo;

/**
 * Fixture for M6 string concatenation: {@code StringBuilder} append chains, both javac's inline
 * concat lowering (compile with {@code -XDstringConcat=inline}) and hand-written chains that must
 * stay calls. Compile with {@code -parameters -g -XDstringConcat=inline}; see ../README.md.
 */
class Sb {
    /** Inline lowering of chunks around a String operand — folds to the original concatenation. */
    String greet(String name) {
        return "Hello, " + name + "!";
    }

    /** A leading chunk and an int operand. */
    String label(int n) {
        return "n = " + n;
    }

    /** A constant char operand compiles to {@code append(C)} of an int — re-rendered as a char. */
    String excl(String s) {
        return s + '!';
    }

    /** A boolean operand (a local, so no re-rendering needed). */
    String flag(String s, boolean b) {
        return s + b;
    }

    /** The empty String operand anchors the chain — recovered verbatim. */
    String seeded(int a, int b) {
        return a + "" + b;
    }

    /** A chain not consumed by toString() re-renders as the original calls. */
    StringBuilder chain(String s) {
        return new StringBuilder().append(s);
    }

    /** A chain consumed by a non-toString call likewise re-renders as calls. */
    int len(String s) {
        return new StringBuilder().append(s).length();
    }

    /** A discarded chain becomes an expression statement, not a dropped value. */
    void drop(String s) {
        new StringBuilder().append(s);
    }

    /** An append on a parameter (not a fresh builder) stays a plain call chain. */
    String manual(StringBuilder sb) {
        return sb.append("x").toString();
    }
}
