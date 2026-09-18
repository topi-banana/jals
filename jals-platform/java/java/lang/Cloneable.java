package java.lang;

/**
 * A marker permitting {@code Object.clone()}.
 *
 * <p>Signature tier, and with no members at all — which is what the JDK declares too. It is here
 * because a program that writes {@code implements Cloneable} is writing correct Java, and a name
 * this package does not declare is a name that resolves nowhere. {@code clone} itself is not
 * declared on {@link Object} here: a module's collector is the embedder's and there is no shallow
 * copy this package could write.
 */
public interface Cloneable {}
