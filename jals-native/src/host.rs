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
//!
//! # What outlives one call
//!
//! A [`RefSlot`] dies with the call that issued it, so a package that must keep something longer —
//! the `Vec` behind a native container, a Java object an element names — asks the host to hold it
//! in a table of its own. [`object_store`](NativeHost::object_store) and
//! [`reference_retain`](NativeHost::reference_retain) put entries in; the `take`/`restore` pairs
//! read them back. The table's lifetime is the run's: it lives exactly as long as the module
//! instance, so nothing can go stale, and it is dropped whole when the run ends.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use crate::value::{HostId, NativeError, NativeValue, RefSlot};

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

    /// Move a Rust object into the host's table and hand back the id a Java field can carry.
    ///
    /// This is what lets a `native` class hold state the JVM side never sees — the `Vec` behind a
    /// native `ArrayList` is the first such object. The table's lifetime is the *run*'s, not the
    /// call's: an entry made while one native method runs is still there when the next one is
    /// called, and is dropped when the run is.
    fn object_store(&mut self, object: Box<dyn Any>) -> Result<HostId, NativeError>;

    /// Take the object out of its slot. It is the caller's until
    /// [`object_restore`](Self::object_restore) puts it back; taking the same id again while it is
    /// out reports [`NativeError::UnknownHandle`].
    fn object_take(&mut self, id: HostId) -> Result<Box<dyn Any>, NativeError>;

    /// Put an object back at its own id — the other half of a take/mutate/restore.
    fn object_restore(&mut self, id: HostId, object: Box<dyn Any>) -> Result<(), NativeError>;

    /// Drop the object. Java has no finalisation on this target, so this is called by whatever
    /// Java method releases the state, or not at all — the whole table is dropped with the run
    /// either way.
    fn object_drop(&mut self, id: HostId) -> Result<(), NativeError>;

    /// Root the reference `slot` names so it survives this call, and hand back a durable id.
    fn reference_retain(&mut self, slot: RefSlot) -> Result<HostId, NativeError>;

    /// A slot of the *current* call naming the retained reference `id`.
    fn reference_restore(&mut self, id: HostId) -> Result<RefSlot, NativeError>;

    /// Let the retained reference go.
    fn reference_release(&mut self, id: HostId) -> Result<(), NativeError>;

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

/// The typed face of the host's table.
///
/// [`NativeHost`] cannot carry generic methods — it is used as `&mut dyn NativeHost`, and a generic
/// method is not object-safe — so the erased methods above are the trait's, and this blanket trait
/// adds the typed vocabulary on top of whichever implementor is in hand. `take::<T>` is a
/// downcast: the wrong type is a refusal naming both, never a silent reinterpretation.
///
/// Implemented for every [`NativeHost`], `?Sized` included, so a binding holding
/// `&mut dyn NativeHost` calls `take::<Vec<_>>` directly.
pub trait HostObjects: NativeHost {
    /// Store `value` and hand back its id.
    fn put<T: 'static>(&mut self, value: T) -> Result<HostId, NativeError> {
        self.object_store(Box::new(value))
    }

    /// Take the object at `id` out, typed.
    ///
    /// A downcast that fails puts the object back before reporting: the type is the caller's
    /// mistake, and losing the state over it would turn a misnamed Rust type into a trap about
    /// nothing.
    fn take<T: 'static>(&mut self, id: HostId) -> Result<T, NativeError> {
        let object = self.object_take(id)?;
        match object.downcast::<T>() {
            Ok(object) => Ok(*object),
            Err(object) => {
                self.object_restore(id, object)?;
                Err(NativeError::HostKind {
                    expected: core::any::type_name::<T>(),
                    found: "a different Rust type",
                })
            }
        }
    }

    /// Put `value` back at `id` — the restore half of a take.
    fn put_back<T: 'static>(&mut self, id: HostId, value: T) -> Result<(), NativeError> {
        self.object_restore(id, Box::new(value))
    }
}

impl<H: NativeHost + ?Sized> HostObjects for H {}
