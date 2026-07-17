//! The top-level `ClassFile` structure (JVMS §4.1) and the entry-point binary codec.

use alloc::vec::Vec;

use serde::{Deserialize, Serialize};

use crate::attribute::Attribute;
use crate::bytes::{Reader, Writer};
use crate::constant_pool::ConstantPool;
use crate::error::{ClassfileError, Result};
use crate::field::FieldInfo;
use crate::flags::ClassAccessFlags;
use crate::method::MethodInfo;

/// The `0xCAFEBABE` magic every class file begins with.
const MAGIC: u32 = 0xCAFE_BABE;

/// A complete, in-memory model of a Java class file (JVMS §4.1).
///
/// The `magic` is validated on [`read`](ClassFile::read) and re-emitted on [`write`](ClassFile::write)
/// but not stored — it is invariant. Counts and byte lengths are likewise not stored; they are
/// derived from the contents on write, which is what makes the round-trip robust against edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassFile {
    /// The minor version number.
    pub minor_version: u16,
    /// The major version number (e.g. 69 for Java 25).
    pub major_version: u16,
    /// The constant pool.
    pub constant_pool: ConstantPool,
    /// The class's access flags.
    pub access_flags: ClassAccessFlags,
    /// `Class` constant-pool index of this class.
    pub this_class: u16,
    /// `Class` constant-pool index of the superclass, or 0 (only for `java.lang.Object` and
    /// `module-info`).
    pub super_class: u16,
    /// `Class` constant-pool indices of the directly-implemented interfaces.
    pub interfaces: Vec<u16>,
    /// The class's fields.
    pub fields: Vec<FieldInfo>,
    /// The class's methods.
    pub methods: Vec<MethodInfo>,
    /// The class's attributes (`SourceFile`, `BootstrapMethods`, …).
    pub attributes: Vec<Attribute>,
}

impl ClassFile {
    /// Parse a class file from any portable byte source ([`jals_storage::io::Read`]) — a
    /// `&[u8]` slice, or a host-side reader bridged through `jals_storage::io::StdReader`.
    /// In-memory sources complete without suspending; the parse yields cooperatively inside
    /// its bulk loops. Returns an [`Err`] (never panics) on any structural problem, including
    /// a bad magic or trailing bytes.
    pub async fn read<R: jals_storage::io::Read>(source: R) -> Result<Self> {
        let mut r = Reader::new(source);
        let magic = r.u32().await?;
        if magic != MAGIC {
            return Err(ClassfileError::BadMagic(magic));
        }
        let minor_version = r.u16().await?;
        let major_version = r.u16().await?;
        let constant_pool = ConstantPool::read(&mut r).await?;
        let access_flags = ClassAccessFlags(r.u16().await?);
        let this_class = r.u16().await?;
        let super_class = r.u16().await?;
        let interfaces = r.u16_list().await?;
        let fields = r
            .list(async |r| FieldInfo::read(r, &constant_pool).await)
            .await?;
        let methods = r
            .list(async |r| MethodInfo::read(r, &constant_pool).await)
            .await?;
        let attributes = Attribute::read_all(&mut r, &constant_pool).await?;
        r.expect_eof().await?;
        Ok(Self {
            minor_version,
            major_version,
            constant_pool,
            access_flags,
            this_class,
            super_class,
            interfaces,
            fields,
            methods,
            attributes,
        })
    }

    /// Serialise this class file back to bytes. For a value parsed by [`read`](ClassFile::read) and
    /// left unmodified, the output is byte-for-byte identical to the input.
    pub fn write(&self) -> Vec<u8> {
        let mut w = Writer::new();
        w.u32(MAGIC);
        w.u16(self.minor_version);
        w.u16(self.major_version);
        self.constant_pool.write(&mut w);
        w.u16(self.access_flags.0);
        w.u16(self.this_class);
        w.u16(self.super_class);
        w.u16_list(&self.interfaces);
        w.list(&self.fields, FieldInfo::write);
        w.list(&self.methods, MethodInfo::write);
        Attribute::write_all(&self.attributes, &mut w);
        w.into_vec()
    }
}
