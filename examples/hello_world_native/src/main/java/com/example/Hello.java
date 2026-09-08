package com.example;

/**
 * The same greeting as {@code hello_world_wasm}, printed rather than spelled out one call at a
 * time.
 *
 * <p>Nothing here is different about the <em>compiler</em>. What changed is that the module imports
 * host functions, because {@code java.io.PrintStream} declares {@code native} methods and the
 * platform package that implements them is linked into this build.
 *
 * <p>{@code System.out.println} itself is <em>Java</em>, compiled into this module beside this
 * class — a {@code char[]} buffer, a growth loop, a flush. The Rust half of the package is twelve
 * functions, and only two of them are on this path: one that takes code units, one that says the
 * text is complete. Everything else a program calls here is Java.
 *
 * <p>There is still no string <em>literal</em>: the backend refuses one, so the greeting is a
 * {@code char[]} wrapped in a {@code String}. That is the one gap left between this target and the
 * language, and it is why every constant in the platform's own Java looks the same way.
 */
public class Hello {

    /** The greeting, held the only way this target can hold a constant string. */
    private static final char[] GREETING = {
        'H', 'e', 'l', 'l', 'o', ',', ' ', 'w', 'o', 'r', 'l', 'd', '!'
    };

    /** Print the greeting. */
    public static void greet() {
        System.out.println(new String(GREETING));
    }

    /**
     * Print one number.
     *
     * <p>{@code Integer.toString} is ordinary Java in the platform, compiled into this module: it
     * accumulates digits negatively so {@code Integer.MIN_VALUE} — which has no positive
     * counterpart — prints correctly, then hands the resulting {@code char[]} across the boundary
     * one flush at a time. So this one call exercises the whole seam: a primitive in, an array read
     * back out by the host.
     */
    public static void printNumber(int value) {
        System.out.println(value);
    }

    /** Count from one to {@code limit}, one line each. */
    public static void countTo(int limit) {
        int at = 1;
        while (at <= limit) {
            System.out.println(at);
            at = at + 1;
        }
    }

    /** A number the host renders, since the shortest round-tripping decimal is its job. */
    public static void printRoot(int value) {
        System.out.println(Math.sqrt(value));
    }
}
