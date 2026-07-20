package evolution;

// Version 2: this other direct superinterface now contributes a distinct override of the
// HierarchyRoot declaration selected by HierarchyLeft.super.
interface HierarchyRight extends HierarchyRoot {
    @Override
    default int rootValue(int value) {
        return value + 30;
    }
}
