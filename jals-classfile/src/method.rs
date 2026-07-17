//! A `method_info` structure (JVMS §4.6).

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::attribute::Attribute;
use crate::bytes::{Input, Reader, Writer};
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
    pub(crate) async fn read<R: Input>(r: &mut Reader<R>, pool: &ConstantPool) -> Result<Self> {
        let access_flags = MethodAccessFlags(r.u16().await?);
        let name_index = r.u16().await?;
        let descriptor_index = r.u16().await?;
        let attributes = Attribute::read_all(r, pool).await?;
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
