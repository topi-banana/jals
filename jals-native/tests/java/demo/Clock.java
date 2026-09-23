package demo;

/** A class whose one method is implemented by the package's Rust half. */
public final class Clock {

    public static native long now();
}
