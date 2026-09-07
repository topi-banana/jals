//! The seam a native function reaches the running module through.
//!
//! This is the one place a package can touch something it did not receive as a number, and it is
//! a trait rather than a concrete type for the reason `jinja`'s `Object` is: the engine is the
//! consumer's, and a crate that named it would stop being a crate a package author can depend on
//! alone. `jals-build`'s tinywasm adapter is this workspace's only implementor.
//!
//! # What a host can and cannot do
//!
//! It can **read and write** a Java array (a wasm array is a first-class object the embedder's
//! collector owns, and its elements are readable by index). It cannot **allocate** one: there is
//! no host-side `struct.new` or `array.new`, so a native method that must hand back an object
//! calls [`call_export`](NativeHost::call_export) and lets the module allocate it.
//!
//! Anything else a reference names — a class instance — is **opaque**. Its field indices are the
//! backend's own layout, and a package that read one would be reading a fact no declaration
//! states.

use alloc::string::String;
use alloc::vec::Vec;

use crate::value::{NativeError, NativeValue, RefSlot};

/// What a native function may ask of the module it was called from.
pub trait NativeHost {
    /// How many elements the array in `slot` holds.
    fn array_len(&mut self, slot: RefSlot) -> Result<u32, NativeError>;

    /// One element. A packed element (Java `boolean`, `byte`, `short`, `char`) comes back as a
    /// zero-extended [`NativeValue::I32`], which is also how the module itself reads one.
    fn array_get(&mut self, slot: RefSlot, index: u32) -> Result<NativeValue, NativeError>;

    /// Write one element. A packed element truncates to its width, exactly as `array.set` does.
    fn array_set(
        &mut self,
        slot: RefSlot,
        index: u32,
        value: NativeValue,
    ) -> Result<(), NativeError>;

    /// Call one of the module's own exported functions and write its results into `results`.
    ///
    /// The escape hatch for everything the host cannot do itself. A native method that must
    /// return a Java object declares a `static` factory in the package's own Java, exports it, and
    /// calls it here — the module allocates, the host computes.
    fn call_export(
        &mut self,
        name: &str,
        args: &[NativeValue],
        results: &mut [NativeValue],
    ) -> Result<(), NativeError>;

    /// Every element of an `int`-shaped array, in order.
    ///
    /// Provided rather than left to each package, because the loop is the same every time and
    /// getting the bounds wrong is a trap rather than a diagnostic.
    fn array_i32(&mut self, slot: RefSlot) -> Result<Vec<i32>, NativeError> {
        let len = self.array_len(slot)?;
        let mut out = Vec::new();
        out.try_reserve(usize::try_from(len).unwrap_or(usize::MAX))
            .map_err(|_| NativeError::Message(String::from("the array is too large to read")))?;
        for index in 0..len {
            let value = self.array_get(slot, index)?;
            out.push(
                value
                    .as_i32()
                    .ok_or_else(|| NativeError::argument(0, "an i32 element", value))?,
            );
        }
        Ok(out)
    }

    /// `count` code units of a Java `char[]` starting at `offset`, decoded as text.
    ///
    /// The single most common thing a package wants from an array, and the one place surrogate
    /// pairs have to be joined — so it is written once here rather than in every package that
    /// prints. An unpaired surrogate becomes `U+FFFD` rather than a refusal: a Java `char[]` is
    /// allowed to hold one, and refusing to print a string because of it is worse than printing
    /// the replacement character a reader already knows how to read.
    fn array_text(
        &mut self,
        slot: RefSlot,
        offset: u32,
        count: u32,
    ) -> Result<String, NativeError> {
        let len = self.array_len(slot)?;
        let end = offset.saturating_add(count);
        if end > len {
            return Err(NativeError::OutOfBounds { index: end, len });
        }
        let mut units = Vec::new();
        units
            .try_reserve(usize::try_from(count).unwrap_or(usize::MAX))
            .map_err(|_| NativeError::Message(String::from("the array is too large to read")))?;
        for index in offset..end {
            let value = self.array_get(slot, index)?;
            let unit = value
                .as_i32()
                .ok_or_else(|| NativeError::argument(0, "a char element", value))?;
            units.push(u16::try_from(unit & 0xFFFF).unwrap_or(0));
        }
        Ok(String::from_utf16_lossy(&units))
    }
}
