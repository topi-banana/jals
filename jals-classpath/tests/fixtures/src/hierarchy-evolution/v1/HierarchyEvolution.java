package evolution;

// Version 1: both qualified interface-super invocations are source-legal. The client class from
// this version is deliberately retained while selected supertypes are replaced by version 2.
interface HierarchyDirect {
    default int directValue(int value) {
        return value + 1;
    }
}

class HierarchyBase {}

interface HierarchyRoot {
    default int rootValue(int value) {
        return value + 2;
    }
}

interface HierarchyLeft extends HierarchyRoot {}

interface HierarchyRight extends HierarchyRoot {}

public class HierarchyEvolution extends HierarchyBase
        implements HierarchyDirect, HierarchyLeft, HierarchyRight {
    @Override
    public int directValue(int value) {
        return value + 10;
    }

    @Override
    public int rootValue(int value) {
        return value + 20;
    }

    public int callDirect(int value) {
        return HierarchyDirect.super.directValue(value);
    }

    public int callLeft(int value) {
        return HierarchyLeft.super.rootValue(value);
    }
}
