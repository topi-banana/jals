package jals.build;

/**
 * The crossing between a script and its host.
 *
 * <p>A {@code native} method cannot receive or return a {@code String}. A host can read a Java
 * {@code char[]} element by element, but a string's representation is the backend's own layout, and
 * a native method reading one would be reading a fact no declaration states — the same reason
 * {@code PrintStream}'s one native method takes a {@code char[]}. Every method here is a step of
 * one of two directions: {@link #of} builds the array for a string going to the host, and a result
 * that is a string comes back as "ask for a length, allocate that many code units, have the host
 * fill them".
 *
 * <p>Results the host cannot hand back in one call pass through one scratch slot at a time. A
 * length query leaves its answer there, and the fill that follows copies it out, so the two calls
 * of one result have to be adjacent — which is what every method of the public API is.
 */
final class Text {

    /** The code units of {@code s}, for a call that has to hand a string to the host. */
    static char[] of(String s) {
        char[] chars = new char[s.length()];
        int i = 0;
        while (i < chars.length) {
            chars[i] = s.charAt(i);
            i = i + 1;
        }
        return chars;
    }

    /** How many code units scratch entry {@code index} holds. */
    static native int entryLength(int index);

    /** Copy scratch entry {@code index} into {@code out}, which has to be its exact length. */
    static native void entryInto(int index, char[] out);
}
