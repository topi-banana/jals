package com.example;

import jals.io.Out;

/**
 * The same greeting as {@code hello_world_wasm}, printed rather than spelled out one call at a
 * time.
 *
 * <p>Nothing here is different about the <em>compiler</em>. There is still no {@code String} and
 * still no {@code System.out}: the text is a {@code char[]}, exactly as it was. What changed is
 * that the module now imports three host functions, because {@code jals.io.Out} declares three
 * {@code native} methods and {@code [build] native-packages} selected the package that implements
 * them.
 *
 * <p>{@code Out.println} itself is <em>Java</em>, compiled into this module beside this class. The
 * Rust half of the package is three functions that buffer code units and hand a decoded string to
 * whatever the host writes with — a terminal for {@code jals}, the Run pane in the browser.
 */
public class Hello {

    /** The greeting, held the only way a WebAssembly module can hold one. */
    private static final char[] GREETING = {
        'H', 'e', 'l', 'l', 'o', ',', ' ', 'w', 'o', 'r', 'l', 'd', '!'
    };

    /** Print the greeting. */
    public static void greet() {
        Out.println(GREETING);
    }

    /**
     * Print one number.
     *
     * <p>{@code Out.printlnInt} is ordinary Java in the package, compiled into this module: it
     * builds the digits into a {@code char[]} and hands that array to a {@code native} method the
     * host reads element by element. So this one call exercises the whole boundary — a primitive
     * in, an array read back out.
     */
    public static void printNumber(int value) {
        Out.printlnInt(value);
    }

    /** Count from one to {@code limit}, one line each. */
    public static void countTo(int limit) {
        int at = 1;
        while (at <= limit) {
            Out.printlnInt(at);
            at = at + 1;
        }
    }
}
