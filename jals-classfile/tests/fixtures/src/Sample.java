import java.util.ArrayList;
import java.util.List;

public class Sample<T extends Comparable<T>> {
    private int count;
    public static final String NAME = "sample";
    private final List<String> items = new ArrayList<>();

    public Sample(int count) {
        this.count = count;
    }

    @Deprecated
    public int getCount() {
        return count;
    }

    public <R> R transform(T input, R fallback) {
        if (count > 0) {
            count--;
        }
        return fallback;
    }

    public void loop() {
        for (int i = 0; i < count; i++) {
            items.add("x" + i);
        }
    }

    enum Kind {
        A,
        B,
        C
    }

    interface Visitor<V> {
        V visit(V value);
    }

    record Point(int x, int y) {}
}
