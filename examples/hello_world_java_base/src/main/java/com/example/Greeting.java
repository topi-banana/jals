package com.example;

/**
 * The same greeting as {@code hello_world_native}, printed through {@code System.out}.
 *
 * <p>Nothing here imports anything: {@code String}, {@code StringBuilder}, {@code Integer},
 * {@code Math}, {@code System} and {@code NumberFormatException} are all {@code java.lang}, which
 * every Java file imports without saying so. What makes them <em>work</em> is
 * {@code [build] native-packages = ["java.base"]} — the package that supplies their bodies,
 * compiled into this same module.
 *
 * <p>The one thing that still looks unusual is {@link #HELLO}. A string literal is not compiled to
 * wasm yet, so a constant string is spelled as a {@code char[]} and wrapped once. Everything after
 * that is ordinary Java.
 */
public final class Greeting {

    /** {@code "Hello, world!"}, spelled the only way this target can spell a constant string. */
    private static final char[] HELLO = {
        'H', 'e', 'l', 'l', 'o', ',', ' ', 'w', 'o', 'r', 'l', 'd', '!'
    };

    /** {@code " -> "}, the separator {@link #describe} puts between a number and its root. */
    private static final char[] ARROW = {' ', '-', '>', ' '};

    /** {@code "12x"}, which is not a number — {@link #parseFailure} is about what happens then. */
    private static final char[] NOT_A_NUMBER = {'1', '2', 'x'};

    /** Never called. */
    private Greeting() {}

    /** Print the greeting. */
    public static void greet() {
        System.out.println(new String(HELLO));
    }

    /**
     * Print {@code value} and its square root.
     *
     * <p>Three parts of the package in one line: {@link StringBuilder} joins, {@link Integer} and
     * {@link Double} render, and {@link Math#sqrt} computes — in Java, by Newton's iteration, with
     * only the bit casts under it coming from the host.
     */
    public static void describe(int value) {
        StringBuilder line = new StringBuilder();
        line.append(Integer.toString(value));
        line.append(new String(ARROW));
        line.append(Double.toString(Math.sqrt(value)));
        System.out.println(line.toString());
    }

    /**
     * Parse text that is not a number, and print what the failure says.
     *
     * <p>A {@code throw} from inside the package, caught by the project. {@code toString} answers
     * with the exception's own class name because every class in the hierarchy overrides the hook
     * that supplies one — there is no {@code getClass()} on this target to read it from.
     */
    public static void parseFailure() {
        try {
            Integer.parseInt(new String(NOT_A_NUMBER));
            System.out.println(new String(HELLO));
        } catch (NumberFormatException failure) {
            System.err.println(failure.toString());
        }
    }
}
