package com.example;

/** Integer arithmetic with its tests beside it, the way a Rust module keeps them. */
public final class Calculator {
    private Calculator() {}

    public static int add(int a, int b) {
        return a + b;
    }

    public static int divide(int numerator, int divisor) {
        return numerator / divisor;
    }

    // A test lives next to what it tests. `jals build` removes it — the class it compiles holds
    // `add` and `divide` and nothing else — and `jals test` keeps it and generates the harness
    // that calls it.
    #[test]
    static void addsTwoNumbers() {
        assert add(2, 3) == 5;
        assert add(-1, 1) == 0;
    }

    // `#[should_fail]` inverts the verdict: this passes only because the body throws.
    #[test]
    #[should_fail]
    static void divisionByZeroThrows() {
        divide(1, 0);
    }

    // `#[ignore]` is listed but not run. `jals test --run-ignored all` runs it anyway.
    #[test]
    #[ignore]
    static void notReadyYet() {
        assert add(1, 1) == 3;
    }
}
