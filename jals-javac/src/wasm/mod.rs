//! The WebAssembly backend: a whole project as one module, with the host's collector managing
//! every object.
//!
//! Java's memory model lands on the [garbage-collection
//! proposal](https://github.com/WebAssembly/gc): a class becomes a `struct` type, inheritance
//! becomes declared subtyping, and `new` becomes `struct.new`. Nothing in this backend traces,
//! marks, sweeps, or frees — allocation hands the object to the embedder's collector, which is the
//! whole point of targeting GC rather than linear memory.
//!
//! Unlike the JVM backend, this one lowers from the syntax tree directly. wasm's control flow is
//! structured (`block` / `loop` / `if`), so the nesting the source already has is the nesting the
//! output needs; going through the other backend's `goto`s would mean recovering it again.

mod encode;
mod insn;
mod lower;

pub use lower::{CompileWasm, WasmError, WasmInput};

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    // Running an emitted module on a real engine lives in `tests/wasm.rs`, which drives the whole
    // compiler rather than a hand-built module — and which may touch the filesystem, as an
    // integration test rather than a portable source file.

    use super::encode::{
        CompType, ExportKind, FieldType, Func, HeapType, Module, RefType, StorageType, SubType,
        ValType,
    };
    use super::insn::{Insn, NumOp};

    /// Whether a tool that understands WebAssembly 3.0 is on this host. Like the JVM-backed tests,
    /// a missing tool skips rather than fails.
    fn tool(name: &str) -> bool {
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// Hand `bytes` to `wasm-tools validate`, which is the specification's own answer to whether a
    /// module is well-formed — the wasm counterpart of letting a real JVM verify a class file.
    fn validate(bytes: &[u8]) {
        if !tool("wasm-tools") {
            return;
        }
        let mut child = Command::new("wasm-tools")
            .arg("validate")
            .arg("-")
            .stdin(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn wasm-tools");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(bytes)
            .expect("write module");
        let output = child.wait_with_output().expect("wasm-tools");
        assert!(
            output.status.success(),
            "wasm-tools rejected the module:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A module whose types form a two-level hierarchy, plus a function that allocates the subtype
    /// and reads a field *through the supertype's* accessor — which only validates because the
    /// subtyping is declared.
    fn hierarchy_module() -> Option<Vec<u8>> {
        let mut module = Module::new();
        let mutable_i32 = FieldType {
            storage: StorageType::Val(ValType::I32),
            mutable: true,
        };
        let mutable_i64 = FieldType {
            storage: StorageType::Val(ValType::I64),
            mutable: true,
        };

        let base = module.add_type(SubType {
            is_final: false,
            supertype: None,
            comp: CompType::Struct(vec![mutable_i32]),
        });
        let sub = module.add_type(SubType {
            is_final: false,
            supertype: Some(base),
            // A subtype's fields start with the supertype's, in order: that prefix is what makes a
            // `(ref $Sub)` readable as a `(ref $Base)` without a conversion.
            comp: CompType::Struct(vec![mutable_i32, mutable_i64]),
        });
        let signature = module.add_type(SubType::plain(CompType::Func {
            params: vec![ValType::I32],
            results: vec![ValType::I32],
        }));

        let mut body = Insn::new();
        // Allocate the subtype, write the parameter plus one into its first field, then read that
        // field back *through the supertype's* accessor — which only validates because the
        // subtyping is declared.
        body.struct_new_default(sub)
            .local_tee(1)
            .local_get(0)
            .i32_const(1)
            .numeric(NumOp::Add, ValType::I32)
            .expect("i32.add")
            .struct_set(sub, 0)
            .local_get(1)
            .struct_get(base, 0);
        module.funcs.push(Func {
            type_index: signature,
            locals: vec![ValType::Ref(RefType::nullable(HeapType::Concrete(sub)))],
            body: body.into_body(),
        });
        let index = Module::func_index(0);
        module
            .exports
            .push(("bump".to_owned(), ExportKind::Func, index));
        module.finish()
    }

    #[test]
    fn a_hand_encoded_gc_module_validates() {
        validate(&hierarchy_module().expect("a module whose lengths all fit"));
    }

    /// LEB128 is where a hand-written encoder goes wrong first: the signed form has to stop on the
    /// sign bit, not on a zero remainder.
    #[test]
    fn the_integer_encodings_round_trip_through_the_spec() {
        use super::encode::Bytes;
        let encode_u32 = |value: u32| {
            let mut bytes = Bytes::new();
            bytes.u32(value);
            bytes.into_vec()
        };
        assert_eq!(encode_u32(0), [0x00]);
        assert_eq!(encode_u32(127), [0x7F]);
        assert_eq!(encode_u32(128), [0x80, 0x01]);
        assert_eq!(encode_u32(624_485), [0xE5, 0x8E, 0x26]);

        let encode_i32 = |value: i32| {
            let mut bytes = Bytes::new();
            bytes.i32(value);
            bytes.into_vec()
        };
        assert_eq!(encode_i32(0), [0x00]);
        assert_eq!(encode_i32(-1), [0x7F]);
        assert_eq!(encode_i32(63), [0x3F]);
        // 64 needs a second byte precisely because 0x40 would read back as -64.
        assert_eq!(encode_i32(64), [0xC0, 0x00]);
        assert_eq!(encode_i32(-64), [0x40]);
        assert_eq!(encode_i32(-123_456), [0xC0, 0xBB, 0x78]);
    }

    /// Unused today, but part of the encoder's surface and cheap to keep honest.
    #[test]
    fn reference_types_encode_their_nullability() {
        use super::encode::Bytes;
        let encode = |ty: ValType| {
            let mut bytes = Bytes::new();
            ty.write(&mut bytes);
            bytes.into_vec()
        };
        // `(ref null $3)` and `(ref $3)` differ only in the leading byte.
        assert_eq!(
            encode(ValType::Ref(RefType::nullable(HeapType::Concrete(3)))),
            [0x63, 0x03]
        );
        assert_eq!(
            encode(ValType::Ref(RefType {
                nullable: false,
                heap: HeapType::Concrete(3),
            })),
            [0x64, 0x03]
        );
        // A concrete heap type is a *signed* LEB, so index 64 needs a second byte where 63 does
        // not — the abstract heap types live in the negative range of the same encoding.
        assert_eq!(
            encode(ValType::Ref(RefType::nullable(HeapType::Concrete(64)))),
            [0x63, 0xC0, 0x00]
        );
    }
}
