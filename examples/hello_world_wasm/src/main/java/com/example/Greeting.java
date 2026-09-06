package com.example;

/**
 * The greeting, held the only way a WebAssembly module can hold one.
 *
 * <p>There is no {@code String} here. Library types have no wasm representation
 * — the module is the whole world, and it contains no {@code java.base} — so
 * the text is a {@code char[]}, which is a wasm array the host's collector
 * allocates and owns.
 */
public class Greeting {
    /** The code units of the greeting, in order. */
    private final char[] text;

    Greeting(char[] text) {
        this.text = text;
    }

    /** How many code units the greeting has. */
    int length() {
        return this.text.length;
    }

    /**
     * The code unit at {@code index}.
     *
     * <p>An index outside the array traps: the bounds check belongs to the host,
     * not to anything this backend emits.
     */
    char at(int index) {
        return this.text[index];
    }
}
