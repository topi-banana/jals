package demo;

// Provenance for InvokeSpecialCalls.class — exercises non-constructor invokespecial dispatch to a
// direct superclass and a direct interface default method, plus explicit super-constructor
// delegation. Compiled with parameter names and local-variable debug information:
//     javac -parameters -g -d out jals-classpath/tests/fixtures/src/InvokeSpecialCalls.java
//     cp out/demo/InvokeSpecialCalls.class jals-classpath/tests/fixtures/
class InvokeSpecialBase {
    InvokeSpecialBase(int seed) {}

    int classValue(int value) {
        return value + 1;
    }
}

interface InvokeSpecialDefault {
    default int interfaceValue(int value) {
        return value + 1;
    }
}

public class InvokeSpecialCalls extends InvokeSpecialBase implements InvokeSpecialDefault {
    public InvokeSpecialCalls(int seed) {
        super(seed);
    }

    @Override
    public int classValue(int value) {
        return value + 2;
    }

    @Override
    public int interfaceValue(int value) {
        return value + 2;
    }

    public int callSuperclass(int value) {
        return super.classValue(value);
    }

    public int callInterface(int value) {
        return demo.InvokeSpecialDefault.super.interfaceValue(value);
    }
}
