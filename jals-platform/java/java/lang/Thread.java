package java.lang;

/**
 * A thread of execution.
 *
 * <p>Signature tier, and not for want of an implementation: a wasm module has one thread, which it
 * does not own, and nothing to start a second one with. This is the record a {@code javac} build's
 * analysis reads, so the JDK type resolves where a project names it.
 */
public class Thread implements Runnable {

    public Thread(Runnable task);

    public Thread(Runnable task, String name);

    public static Thread currentThread();

    public static void sleep(long millis) throws InterruptedException;

    public static boolean interrupted();

    public void start();

    @Override
    public void run();

    public void interrupt();

    public boolean isInterrupted();

    public final boolean isAlive();

    public final void setDaemon(boolean on);

    public final boolean isDaemon();

    public final String getName();

    public final void join() throws InterruptedException;
}
