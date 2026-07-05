package demo;

// Provenance for Locals.class — exercises M3 local-variable reconstruction: a straight-line body
// with temporaries, a local assigned in both branches and read after the join (the hoisting case),
// and a reference-typed local. Compiled with `javac` (JDK 25) with -g so the LocalVariableTable is
// present (M3 reads local names / types from it):
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Locals.java
//     cp out/demo/Locals.class jals-classpath/tests/fixtures/
public class Locals {
    // Straight-line locals: two temporaries, each read once.
    public int compute(int n) {
        int doubled = n * 2;
        int result = doubled + 1;
        return result;
    }

    // A local written in both branches and read after the join — hoisting keeps `x` in scope.
    public int pick(boolean c) {
        int x;
        if (c) {
            x = 1;
        } else {
            x = 2;
        }
        return x;
    }

    // A reference-typed local (astore) then a read.
    public int nameLength(String s) {
        String t = s;
        return t.length();
    }
}
