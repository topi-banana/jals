package java.lang;

/**
 * The diagnostics a declaration should not be reported for.
 *
 * <p>{@code jals-lint} reads this one out of the syntax tree rather than through the index — a
 * suppression has to apply before the analysis that would produce the finding — so this
 * declaration does not make suppression work. It makes {@code @SuppressWarnings} <em>resolve</em>,
 * which is a different question with the same answer for every other annotation in this package.
 */
public @interface SuppressWarnings {

    /** A rule name, a section name, or {@code "all"}. */
    String[] value();
}
