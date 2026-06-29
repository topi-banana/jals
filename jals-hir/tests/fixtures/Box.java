// Provenance for Box.class — a generic class used by tests/classpath.rs to check that the classpath
// bridge resolves members and substitutes generics. Compiled with `javac` (JDK 25):
//     javac -d jals-hir/tests/fixtures jals-hir/tests/fixtures/Box.java
public class Box<T> {
    private T value;

    public T get() {
        return value;
    }

    public void set(T value) {
        this.value = value;
    }
}
