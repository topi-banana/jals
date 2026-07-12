//! A `method_info` structure (JVMS §4.6).

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::attribute::Attribute;
use crate::bytes::{Reader, Writer};
use crate::constant_pool::ConstantPool;
use crate::error::Result;
use crate::flags::MethodAccessFlags;

/// A method declared by a class (JVMS §4.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MethodInfo {
    /// The method's access flags.
    pub access_flags: MethodAccessFlags,
    /// `Utf8` constant-pool index of the method's simple name (`<init>` / `<clinit>` for the special
    /// methods).
    pub name_index: u16,
    /// `Utf8` constant-pool index of the method's descriptor.
    pub descriptor_index: u16,
    /// The method's attributes (`Code`, `Exceptions`, `Signature`, …).
    pub attributes: Vec<Attribute>,
}

impl MethodInfo {
    pub(crate) fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Self> {
        let access_flags = MethodAccessFlags(r.u16()?);
        let name_index = r.u16()?;
        let descriptor_index = r.u16()?;
        let attributes = Attribute::read_all(r, pool)?;
        Ok(Self {
            access_flags,
            name_index,
            descriptor_index,
            attributes,
        })
    }

    pub(crate) fn write(&self, w: &mut Writer) {
        w.u16(self.access_flags.0);
        w.u16(self.name_index);
        w.u16(self.descriptor_index);
        Attribute::write_all(&self.attributes, w);
    }
}
