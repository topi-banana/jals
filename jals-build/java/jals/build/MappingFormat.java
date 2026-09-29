package jals.build;

/**
 * The grammar a mappings file is written in: the third argument of {@link Tasks#remapJarAs}.
 *
 * <p>A value rather than a pair of loose strings because a namespace pair means nothing without
 * the format that names it — {@link Tasks#tinyV2} is the only way to write one, so a script cannot
 * pair namespaces with a grammar that has none.
 *
 * <p>A value is also a place a mistake can be reported: {@link Tasks#tinyV2} asks the host whether
 * the pair it was handed could rename anything, so an unusable grammar stops the script at the
 * call that wrote it and names what is wrong. {@link Tasks#remapJarAs} checks again what it is
 * handed, because a value can be held, copied, or never used at all.
 */
public final class MappingFormat {

    /** The grammar tag: 0 is ProGuard-style text, 1 is tiny v2. */
    final int kind;

    /** The namespace a deobfuscating tiny v2 remap reads names from, or "" for ProGuard. */
    final String from;

    /** The namespace it writes names to, or "" for ProGuard. */
    final String to;

    MappingFormat(int kind, String from, String to) {
        this.kind = kind;
        this.from = from;
        this.to = to;
    }
}
