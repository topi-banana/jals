package evolution;

// Version 2: the old client still lists HierarchyDirect directly, but it is now also inherited
// through the direct superclass. Re-rendering HierarchyDirect.super is therefore illegal.
class HierarchyBase implements HierarchyDirect {}
