package com.example;

/**
 * The Java convention: a separate tree, named in `[test] source-dirs`. Both places work and they
 * are additive, exactly as on the JVM side — the runner is what differs, not where tests live.
 */
public final class OverflowTest {
    /**
     * Written out rather than read from {@code Integer.MAX_VALUE}: there is no {@code java.base}
     * in a module, so a library constant is a type this backend cannot spell.
     */
    private static final int MAX_VALUE = 2147483647;

    /** {@code -2147483648} is not writable as a literal — the magnitude alone is out of range. */
    private static final int MIN_VALUE = -2147483647 - 1;

    #[test]
    static void additionWrapsLikeJava() {
        assert Calculator.add(MAX_VALUE, 1) == MIN_VALUE;
    }

    /** Listed but not run. `jals test --run-ignored all` runs it anyway — and it fails. */
    #[test]
    #[ignore]
    static void notReadyYet() {
        assert Calculator.add(MAX_VALUE, 1) == MAX_VALUE;
    }
}
