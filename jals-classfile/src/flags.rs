//! Access-flag bit sets for classes, fields, and methods (JVMS §4.1 / §4.5 / §4.6).
//!
//! Each is a newtype over the raw `u16`, so the codec round-trips the exact value (unknown bits
//! survive). Named constants and `is_*` helpers cover the flags downstream consumers need.

use serde::{Deserialize, Serialize};

/// Defines an access-flag newtype with named bit constants, a `contains` helper, and any extra
/// `is_*` predicates supplied after the constant block.
macro_rules! access_flags {
    (
        $(#[$meta:meta])*
        $name:ident { $($(#[$cmeta:meta])* const $flag:ident = $val:literal;)* }
        $($helper:item)*
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub u16);

        impl $name {
            $($(#[$cmeta])* pub const $flag: u16 = $val;)*

            /// Whether every bit set in `mask` is also set here.
            pub const fn contains(self, mask: u16) -> bool {
                self.0 & mask == mask
            }

            $($helper)*
        }
    };
}

access_flags! {
    /// Access flags for a class or interface (JVMS Table 4.1-B).
    ClassAccessFlags {
        const PUBLIC = 0x0001;
        const FINAL = 0x0010;
        const SUPER = 0x0020;
        const INTERFACE = 0x0200;
        const ABSTRACT = 0x0400;
        const SYNTHETIC = 0x1000;
        const ANNOTATION = 0x2000;
        const ENUM = 0x4000;
        const MODULE = 0x8000;
    }
    /// Whether `ACC_INTERFACE` is set.
    pub const fn is_interface(self) -> bool { self.contains(Self::INTERFACE) }
    /// Whether `ACC_ANNOTATION` is set.
    pub const fn is_annotation(self) -> bool { self.contains(Self::ANNOTATION) }
    /// Whether `ACC_ENUM` is set.
    pub const fn is_enum(self) -> bool { self.contains(Self::ENUM) }
    /// Whether `ACC_ABSTRACT` is set.
    pub const fn is_abstract(self) -> bool { self.contains(Self::ABSTRACT) }
    /// Whether `ACC_MODULE` is set.
    pub const fn is_module(self) -> bool { self.contains(Self::MODULE) }
}

access_flags! {
    /// Access flags for a field (JVMS Table 4.5-A).
    FieldAccessFlags {
        const PUBLIC = 0x0001;
        const PRIVATE = 0x0002;
        const PROTECTED = 0x0004;
        const STATIC = 0x0008;
        const FINAL = 0x0010;
        const VOLATILE = 0x0040;
        const TRANSIENT = 0x0080;
        const SYNTHETIC = 0x1000;
        const ENUM = 0x4000;
    }
    /// Whether `ACC_STATIC` is set.
    pub const fn is_static(self) -> bool { self.contains(Self::STATIC) }
    /// Whether `ACC_PUBLIC` is set.
    pub const fn is_public(self) -> bool { self.contains(Self::PUBLIC) }
    /// Whether `ACC_ENUM` is set.
    pub const fn is_enum(self) -> bool { self.contains(Self::ENUM) }
}

access_flags! {
    /// Access flags for a method (JVMS Table 4.6-A).
    MethodAccessFlags {
        const PUBLIC = 0x0001;
        const PRIVATE = 0x0002;
        const PROTECTED = 0x0004;
        const STATIC = 0x0008;
        const FINAL = 0x0010;
        const SYNCHRONIZED = 0x0020;
        const BRIDGE = 0x0040;
        const VARARGS = 0x0080;
        const NATIVE = 0x0100;
        const ABSTRACT = 0x0400;
        const STRICT = 0x0800;
        const SYNTHETIC = 0x1000;
    }
    /// Whether `ACC_STATIC` is set.
    pub const fn is_static(self) -> bool { self.contains(Self::STATIC) }
    /// Whether `ACC_PUBLIC` is set.
    pub const fn is_public(self) -> bool { self.contains(Self::PUBLIC) }
    /// Whether `ACC_VARARGS` is set.
    pub const fn is_varargs(self) -> bool { self.contains(Self::VARARGS) }
    /// Whether `ACC_ABSTRACT` is set.
    pub const fn is_abstract(self) -> bool { self.contains(Self::ABSTRACT) }
}
