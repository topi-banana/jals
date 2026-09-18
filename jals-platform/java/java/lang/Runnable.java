package java.lang;

/** A task with no argument and no result — what a {@code () -> { ... }} lambda is most often. */
@FunctionalInterface
public interface Runnable {

    /** Do the work. */
    void run();
}
