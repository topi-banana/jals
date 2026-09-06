package com.example;

/**
 * Integer arithmetic with its tests beside it — the Rust model, and here also the whole language
 * subset a wasm test can use: primitives and project-declared classes. There is no {@code String},
 * no boxing and no {@code System.out} on this target, so a test says what it means with
 * {@code assert} and nothing else.
 */
public final class Calculator {
    public static int add(int a, int b) {
        return a + b;
    }

    public static int divide(int a, int b) {
        return a / b;
    }

    #[test]
    static void addsTwoNumbers() {
        assert add(2, 3) == 5;
    }

    /**
     * `assert` is compiled into a check only for a test run. An ordinary `jals build` emits
     * nothing for it, exactly as a JVM ignores one unless it was started with `-ea` — and a
     * module has no `-ea`, so the decision is the compile's.
     */
    #[test]
    static void addingIsCommutative() {
        assert add(2, 3) == add(3, 2);
    }

    /**
     * A failing test signals with a trap: dividing by zero traps in wasm exactly as it throws on a
     * JVM, and `#[should_fail]` inverts either one. The inversion is the runner's — a module
     * cannot catch this, because a `catch` type has to be a class the project declares.
     */
    #[test]
    #[should_fail]
    static void dividingByZeroFails() {
        divide(1, 0);
    }
}
