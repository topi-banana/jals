package com.example;

/**
 * The Java convention: a separate tree, named in `[test] source-dirs`. Compiled only by
 * `jals test`, so nothing here reaches `target/classes`.
 */
public final class CalculatorTest {
    #[test]
    static void addsAcrossTheTree() {
        assert Calculator.add(20, 22) == 42;
    }

    #[test]
    static void dividesExactly() {
        assert Calculator.divide(84, 2) == 42;
    }
}
