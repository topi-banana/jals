package java.lang;

/**
 * The arithmetic a script reaches for between two numbers.
 *
 * <p>{@code max}, {@code min} and {@code abs} are the integer ones, and they are total: {@code abs}
 * of {@code Integer.MIN_VALUE} is itself, because the negation of the most negative {@code int} is
 * that same {@code int} and the JDK says so rather than throwing.
 *
 * <p>{@code sqrt} is Newton's method. It is not the hardware instruction the JDK calls, but it is
 * a function of the value alone — no table, no host, no iteration cap — and it answers the corners
 * the way IEEE 754 defines them: {@code NaN} and a negative input are {@code NaN}, and {@code 0.0},
 * {@code -0.0} and {@code +infinity} are their own square roots. The iteration runs until the next
 * estimate is the current one, which is the point where the last bit stopped moving.
 */
public class Math extends Object {

    public static int max(int a, int b) {
        return a >= b ? a : b;
    }

    public static int min(int a, int b) {
        return a <= b ? a : b;
    }

    public static int abs(int a) {
        return a < 0 ? -a : a;
    }

    public static double sqrt(double a) {
        if (a != a) {
            return a;
        }
        if (a < 0.0) {
            return 0.0 / 0.0;
        }
        if (a == 0.0) {
            return a;
        }
        if (a == 1.0 / 0.0) {
            return a;
        }
        double estimate = a;
        double previous = 0.0;
        while (estimate != previous) {
            previous = estimate;
            estimate = (estimate + a / estimate) * 0.5;
        }
        return estimate;
    }
}
