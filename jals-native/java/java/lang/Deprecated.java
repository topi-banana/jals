package java.lang;

/** A declaration that should no longer be used. */
public @interface Deprecated {

    /** The release it stopped being recommended in, or the empty string. */
    String since() default "";

    /** Whether it is scheduled to be removed. */
    boolean forRemoval() default false;
}
