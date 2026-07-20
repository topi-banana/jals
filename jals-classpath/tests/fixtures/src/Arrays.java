package demo;

// Provenance for Arrays.class — exercises M5 array operations: element reads/writes (iaload /
// iastore), newarray/anewarray/multianewarray creation, folded array initializers (int, String,
// long, boolean, nested), an array-typed checkcast, array class literals, arraylength, and a compound
// element store (xs[i]++, which compiles to dup2) that must bail to the safe body.
// Compiled with `javac` (JDK 25) with -parameters -g:
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/Arrays.java
//     cp out/demo/Arrays.class jals-classpath/tests/fixtures/
public class Arrays {
    // Element read (iaload).
    public int first(int[] xs) {
        return xs[0];
    }

    // Element write (iastore) with a non-constant index.
    public void put(int[] xs, int i, int v) {
        xs[i] = v;
    }

    // Dynamic-length primitive array (newarray) — no initializer possible.
    public int[] fill(int n) {
        return new int[n];
    }

    // Dynamic-length object array (anewarray).
    public String[] blank(int n) {
        return new String[n];
    }

    // Zero-length array — constant length, no stores.
    public int[] none() {
        return new int[0];
    }

    // Folded int[] initializer (newarray + dup/iconst/iastore runs).
    public int[] pair() {
        return new int[]{1, 2};
    }

    // Folded String[] initializer (aastore + ldc values).
    public String[] tags() {
        return new String[]{"x", "y"};
    }

    // Folded long[] initializer — a category-2 element value is still one stack expression.
    public long[] wide(long v) {
        return new long[]{v};
    }

    // Folded boolean[] initializer — bastore of iconst_1/iconst_0 mapped back to true/false.
    public boolean[] flags() {
        return new boolean[]{true, false};
    }

    // An initializer stored to a local, then element reads.
    public int firstTwo() {
        int[] xs = new int[]{3, 4};
        return xs[0] + xs[1];
    }

    // arraylength on a folded creation (the creation, as a receiver, needs parentheses).
    public int lenNew() {
        return new int[]{7}.length;
    }

    // arraylength on a parameter.
    public int len(String[] xs) {
        return xs.length;
    }

    // Array-typed checkcast.
    public int[] narrow(Object o) {
        return (int[]) o;
    }

    // multianewarray with two sized dimensions.
    public int[][] grid(int a, int b) {
        return new int[a][b];
    }

    // anewarray of an array class ([I): one sized dimension, one empty.
    public int[][] rows(int n) {
        return new int[n][];
    }

    // ldc of an array Class constant uses a field descriptor in the constant pool.
    public Class<?> primitiveArrayClass() {
        return int[].class;
    }

    public Class<?> referenceArrayClass() {
        return String[].class;
    }

    public Class<?> multidimensionalArrayClass() {
        return String[][].class;
    }

    // Nested initializer: anewarray [I collecting inner folded int[] values.
    public int[][] nested() {
        return new int[][] {{1}, {2}};
    }

    // Must BAIL: a compound element store compiles to dup2 (not modelled).
    public void bump(int[] xs, int i) {
        xs[i]++;
    }
}
