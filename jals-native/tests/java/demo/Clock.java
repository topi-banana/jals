package demo;

/** A fixture for `java_package!`: one `native` method, and one method calling it. */
public final class Clock {

    private Clock() {
    }

    /** The host's reading. */
    public static native long now();

    /** The host's reading, doubled — so the fixture has a body as well as a binding. */
    public static long twice() {
        return now() * 2;
    }
}
