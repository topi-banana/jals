package com.example;

/**
 * The module's exported surface.
 *
 * <p>Every {@code static} method that is not a constructor becomes a wasm
 * export, so these two are what {@code jals run --invoke} can reach. An export
 * name carries no owner — {@code greetingLength}, not
 * {@code com.example.Hello.greetingLength} — which is why they are named for
 * what they answer rather than for the class holding them.
 *
 * <p>Neither returns the greeting itself. A {@code char[]} is a reference, and
 * a reference is an object the host's collector owns: there is nothing outside
 * the engine to print. Turning code units back into text is the caller's half
 * of the job, and the README shows it.
 */
public class Hello {
    /**
     * Module state, which is what a wasm global is.
     *
     * <p>Its value is a {@code new}, and a global's initialiser is a constant
     * expression that cannot contain one — so the allocation happens in the
     * static initialiser below, which this backend lowers into the module's
     * start function. That is what makes {@code jals run} with no
     * {@code --invoke} do something: instantiating a module runs its start
     * function.
     */
    private static final Greeting GREETING;

    static {
        GREETING =
            new Greeting(
                new char[]{'H', 'e', 'l', 'l', 'o', ',', ' ', 'w', 'o', 'r', 'l', 'd', '!'});
    }

    /** How many code units the greeting has. */
    public static int greetingLength() {
        return GREETING.length();
    }

    /** The code unit at {@code index}, as an {@code int} the caller can render. */
    public static int greetingCharAt(int index) {
        return GREETING.at(index);
    }
}
