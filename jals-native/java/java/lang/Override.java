package java.lang;

/**
 * A declaration that overrides one it inherits.
 *
 * <p>Carries no meta-annotation. {@code @Retention} and {@code @Target} live in
 * {@code java.lang.annotation}, which this package does not declare — nothing on this target reads
 * an annotation at run time, so a retention policy would describe a reader that does not exist.
 * What this declaration is for is that {@code @Override} <em>resolves</em>, which is what keeps a
 * file that writes it free of a diagnostic about a type that is really there.
 */
public @interface Override {
}
