//! The Rust half of `java.util.ArrayList`: a `Vec` behind a handle.
//!
//! This is the crate's first **native class**, and the shape a package author copies: a Java class
//! owns an `int` handle, its constructor calls a `static native int allocate()`, and every method
//! is a `static native` one taking the handle as its first parameter. The typed registration in
//! [`jals_native`] hides the handle and hands each binding `&mut Elements`, so nothing below
//! matches on an argument position for the receiver.
//!
//! # Elements outlive the call
//!
//! A `Vec<HostValue>` is the whole storage. An element is a Java reference, and a reference names
//! something for exactly one call — so `add` asks the host to **retain** it and stores the id the
//! host gave back, and `get` asks the host to restore that id to a slot of *this* call. The object
//! the caller gets back is the object it added, however many calls apart, because the host's table
//! roots it in the engine's collector for the run's lifetime.
//!
//! # Java does the bounds checks
//!
//! `ArrayList.get` throws `IndexOutOfBoundsException` before it calls `getElement`, so a program
//! can catch what it would catch on a JVM. The refusals here are the second line: a binding that
//! is driven directly — by this crate's own tests, or by a Java half that disagreed with it — gets
//! a named refusal rather than an index past the end.

use alloc::vec::Vec;

use jals_native::{HostValue, JavaPackage, NativeError, NativeHost, NativeValue};

/// The vector behind one `java.util.ArrayList`.
struct Elements {
    items: Vec<HostValue>,
}

impl Elements {
    /// The `usize` position `index` names, refused when it is not one.
    fn position(&self, index: i32) -> Result<usize, NativeError> {
        let length = u32::try_from(self.items.len()).unwrap_or(u32::MAX);
        let position = u32::try_from(index)
            .ok()
            .filter(|position| *position < length)
            .ok_or_else(|| NativeError::OutOfBounds {
                index: index.cast_unsigned(),
                len: length,
            })?;
        usize::try_from(position).map_err(|_| {
            NativeError::Message(alloc::string::String::from(
                "the list is larger than this host can index",
            ))
        })
    }

    /// The position `index` names for an insertion, which may be one past the end.
    fn insertion(&self, index: i32) -> Result<usize, NativeError> {
        let length = u32::try_from(self.items.len()).unwrap_or(u32::MAX);
        let position = u32::try_from(index)
            .ok()
            .filter(|position| *position <= length)
            .ok_or_else(|| NativeError::OutOfBounds {
                index: index.cast_unsigned(),
                len: length,
            })?;
        usize::try_from(position).map_err(|_| {
            NativeError::Message(alloc::string::String::from(
                "the list is larger than this host can index",
            ))
        })
    }

    /// Let go of an element the list no longer holds, so the host's table does not keep it alive.
    fn discard(host: &mut dyn NativeHost, value: HostValue) -> Result<(), NativeError> {
        if let HostValue::Reference(id) = value {
            host.reference_release(id)?;
        }
        Ok(())
    }
}

/// Installs `java.util.ArrayList`'s native methods onto a package.
pub struct Collections;

impl Collections {
    /// Bind every `native` method the list's Java declares.
    ///
    /// Seven, and every one of them is an operation the Java half cannot perform on a `Vec` it
    /// cannot see. `indexOf`, `contains` and `remove(Object)` are *not* here: they are loops over
    /// `getElement`, written in Java, because their equality is `Object.equals` and that is a
    /// dispatch the module already performs.
    pub fn install(package: &mut JavaPackage) {
        package
            .native_class::<Elements>("java/util/ArrayList")
            .allocate("allocate()I", |_| Ok(Elements { items: Vec::new() }))
            .method("sizeOf(I)I", |list, _host, _args, mut results| {
                results.set(
                    0,
                    NativeValue::I32(i32::try_from(list.items.len()).unwrap_or(i32::MAX)),
                );
                Ok(())
            })
            .method(
                "addElement(ILjava/lang/Object;)Z",
                |list, host, args, mut results| {
                    let element = HostValue::capture(host, args.value(0)?)?;
                    list.items.push(element);
                    results.set(0, NativeValue::I32(1));
                    Ok(())
                },
            )
            .method(
                "insertElement(IILjava/lang/Object;)V",
                |list, host, args, _results| {
                    let position = list.insertion(args.i32(0)?)?;
                    let element = HostValue::capture(host, args.value(1)?)?;
                    list.items.insert(position, element);
                    Ok(())
                },
            )
            .method(
                "getElement(II)Ljava/lang/Object;",
                |list, host, args, mut results| {
                    let position = list.position(args.i32(0)?)?;
                    let value = list.items[position].restore(host)?;
                    results.set(0, value);
                    Ok(())
                },
            )
            .method(
                "setElement(IILjava/lang/Object;)Ljava/lang/Object;",
                |list, host, args, mut results| {
                    let position = list.position(args.i32(0)?)?;
                    let element = HostValue::capture(host, args.value(1)?)?;
                    let previous = core::mem::replace(&mut list.items[position], element);
                    results.set(0, previous.restore(host)?);
                    Elements::discard(host, previous)?;
                    Ok(())
                },
            )
            .method(
                "removeElement(II)Ljava/lang/Object;",
                |list, host, args, mut results| {
                    let position = list.position(args.i32(0)?)?;
                    let removed = list.items.remove(position);
                    results.set(0, removed.restore(host)?);
                    Elements::discard(host, removed)?;
                    Ok(())
                },
            );
    }
}
