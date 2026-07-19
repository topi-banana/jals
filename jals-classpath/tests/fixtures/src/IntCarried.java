package demo;

// Provenance for IntCarried.class — exercises boolean and char values carried by the JVM's int
// instructions through return, local, field, call, array, and conditional contexts. Compiled with
// `javac` (JDK 25) with parameter names and local-variable debug information:
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/IntCarried.java
//     cp out/demo/IntCarried.class jals-classpath/tests/fixtures/
public class IntCarried {
    public static final char CONSTANT_CHAR = 'G';

    private boolean flag;
    private char letter;

    public boolean booleanReturn() {
        return true;
    }

    public char charReturn() {
        return 'A';
    }

    public boolean booleanLocal() {
        boolean value = true;
        return value;
    }

    public char charLocal() {
        char value = 'B';
        return value;
    }

    public void storeFields() {
        this.flag = true;
        this.letter = 'C';
    }

    public boolean readFlag() {
        return this.flag;
    }

    public char readLetter() {
        return this.letter;
    }

    public boolean passBoolean(boolean value) {
        return value;
    }

    public char passChar(char value) {
        return value;
    }

    public boolean callBoolean() {
        return passBoolean(true);
    }

    public char callChar() {
        return passChar('D');
    }

    private int charOrInt(char value) {
        return 1;
    }

    private int charOrInt(int value) {
        return 2;
    }

    public int widenedCharCall(char value) {
        return charOrInt((int) value);
    }

    public String widenedCharConcat(char value) {
        return "" + (int) value;
    }

    public int branchOnCall(boolean value) {
        if (!passBoolean(value)) {
            return 1;
        }
        return 2;
    }

    public boolean[] booleanArray() {
        return new boolean[]{true, false};
    }

    public char[] charArray() {
        return new char[]{'E', (char) 0xD800};
    }

    public void storeArrays(boolean[] flags, char[] letters) {
        flags[0] = true;
        letters[0] = 'F';
    }

    public boolean readBoolean(boolean[] values) {
        return values[0];
    }

    public char readChar(char[] values) {
        return values[0];
    }

    public int integerZero(int value) {
        if (value == 0) {
            return 1;
        }
        return 2;
    }

    public int booleanNegation(boolean value) {
        if (!value) {
            return 1;
        }
        return 2;
    }

    public char castChar(int value) {
        return (char) value;
    }

    public char surrogate() {
        return (char) 0xD800;
    }
}
