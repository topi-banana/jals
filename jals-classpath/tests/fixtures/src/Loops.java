package demo;

// Provenance for Loops.class — exercises M4 loop structuring: a bottom-test `while` with an `iinc`
// counter and a `do`-`while`. Compiled with `javac` (JDK 25) with -g so local names / types are in
// the LocalVariableTable (loop bodies rely on M3 local-variable reconstruction):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Loops.java
//     cp out/demo/Loops.class jals-classpath/tests/fixtures/
public class Loops {
    // A bottom-test `while` (javac's default loop layout) with a loop counter.
    public int sum(int n) {
        int total = 0;
        int i = 0;
        while (i < n) {
            total = total + i;
            i = i + 1;
        }
        return total;
    }

    // A `do`-`while` (the condition is tested at the bottom, body always runs once).
    public int count(int n) {
        int c = 0;
        do {
            c = c + 1;
        } while (c < n);
        return c;
    }
}
