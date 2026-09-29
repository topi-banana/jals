package java.lang;

/**
 * The assembly the two floating-point renderers share.
 *
 * <p>{@code Double} and {@code Float} differ in where their digits come from and in nothing else:
 * the JDK's notation is one rule over a digit string, the place of its first digit, and a sign.
 * Keeping that rule here is what stops the two classes from drifting into rendering the same
 * number differently.
 *
 * <p>With the value equal to {@code d1.d2... × 10^point} — one to seventeen digits, no trailing
 * zero — the plain form is used exactly when {@code -3 <= point < 7}, and the scientific form
 * otherwise. Both forms keep at least one digit after the point ({@code 1.0E7}, never
 * {@code 1E7}), because a rendering that is not the JDK's is one a program comparing strings will
 * eventually find.
 *
 * <p>Package-private, like the JDK's class of the same name: it is an implementation detail of
 * {@code java.lang}, not part of the surface a program compiles against.
 */
class FloatingDecimal extends Object {

    private FloatingDecimal() {}

    /** Assemble the JDK rendering of a sign and a digit string whose first digit is at `point`. */
    static String toJavaFormatString(boolean negative, char[] digits, int length, int point) {
        StringBuilder out = new StringBuilder();
        if (negative) {
            out.append('-');
        }
        if (point >= -3 && point < 7) {
            if (point >= 0) {
                int i = 0;
                while (i <= point) {
                    if (i < length) {
                        out.append(digits[i]);
                    } else {
                        out.append('0');
                    }
                    i = i + 1;
                }
                out.append('.');
                if (length > point + 1) {
                    while (i < length) {
                        out.append(digits[i]);
                        i = i + 1;
                    }
                } else {
                    out.append('0');
                }
            } else {
                out.append("0.");
                int i = 0;
                while (i < -point - 1) {
                    out.append('0');
                    i = i + 1;
                }
                i = 0;
                while (i < length) {
                    out.append(digits[i]);
                    i = i + 1;
                }
            }
        } else {
            out.append(digits[0]);
            out.append('.');
            if (length > 1) {
                int i = 1;
                while (i < length) {
                    out.append(digits[i]);
                    i = i + 1;
                }
            } else {
                out.append('0');
            }
            out.append('E');
            out.append(point);
        }
        return out.toString();
    }
}
