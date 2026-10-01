package jals.build;

/**
 * The project the script is building, as one read-only tree.
 *
 * <p>Every path is a portable project-relative key, bounded in bytes and segments by the same
 * limits the declarative API enforces, and the tree is the immutable snapshot the build resolved
 * before the script ran. Nothing here writes: a file the script produces goes through
 * {@link Output}, and what the project does with it is the script's {@link Build#addSource} call.
 */
public final class Project {

    /** Every byte of the project file at {@code path}, as a {@code 0..=255} array. */
    public static byte[] read(String path) {
        char[] name = Text.of(path);
        int size = readSize(name);
        byte[] bytes = new byte[size];
        readInto(name, bytes);
        return bytes;
    }

    /** The UTF-8 text of the project file at {@code path}. */
    public static String readText(String path) {
        char[] name = Text.of(path);
        int size = readTextSize(name);
        char[] chars = new char[size];
        readTextInto(name, chars);
        return new String(chars);
    }

    /** Whether a project-relative file or directory exists at {@code path}. */
    public static boolean exists(String path) {
        return exists0(Text.of(path));
    }

    /** The direct children of the project directory at {@code path}, in deterministic order. */
    public static String[] readDir(String path) {
        return entries(readDirCount(Text.of(path)));
    }

    /** Every file below the project directory at {@code path}, in deterministic order. */
    public static String[] walkFiles(String path) {
        return entries(walkFilesCount(Text.of(path)));
    }

    /**
     * The scratch entries a listing query left, as strings.
     *
     * <p>Package-private so {@link Build#features()} can reuse it: one host buffer holds whichever
     * string list the most recent count query produced, and every reader copies out of it
     * immediately.
     */
    static String[] entries(int count) {
        String[] out = new String[count];
        int i = 0;
        while (i < count) {
            int length = Text.entryLength(i);
            char[] chars = new char[length];
            Text.entryInto(i, chars);
            out[i] = new String(chars);
            i = i + 1;
        }
        return out;
    }

    private static native int readSize(char[] path);

    private static native void readInto(char[] path, byte[] out);

    private static native int readTextSize(char[] path);

    private static native void readTextInto(char[] path, char[] out);

    private static native boolean exists0(char[] path);

    private static native int readDirCount(char[] path);

    private static native int walkFilesCount(char[] path);
}
