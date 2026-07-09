//! A `field_info` structure (JVMS §4.5).

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::attribute::{self, Attribute};
use crate::bytes::{Reader, Writer};
use crate::constant_pool::ConstantPool;
use crate::error::Result;
use crate::flags::FieldAccessFlags;

/// A field declared by a class (JVMS §4.5).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FieldInfo {
    /// The field's access flags.
    pub access_flags: FieldAccessFlags,
    /// `Utf8` constant-pool index of the field's simple name.
    pub name_index: u16,
    /// `Utf8` constant-pool index of the field's descriptor.
    pub descriptor_index: u16,
    /// The field's attributes (`ConstantValue`, `Signature`, annotations, …).
    pub attributes: Vec<Attribute>,
}

impl FieldInfo {
    pub(crate) fn read(r: &mut Reader<'_>, pool: &ConstantPool) -> Result<Self> {
        let access_flags = FieldAccessFlags(r.u16()?);
        let name_index = r.u16()?;
        let descriptor_index = r.u16()?;
        let attributes = attribute::read_attributes(r, pool)?;
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
        attribute::write_attributes(&self.attributes, w);
    }
}
