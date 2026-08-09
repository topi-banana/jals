//! Java to one WebAssembly module.
//!
//! # Memory management is the host's
//!
//! A Java class becomes a wasm `struct` type and `new` becomes `struct.new`; inheritance becomes
//! *declared* subtyping, so a `(ref $Sub)` is usable wherever a `(ref $Super)` is expected without
//! a conversion or a header word. Nothing in this backend allocates linear memory, keeps a free
//! list, traces, or frees — the embedder's collector owns every object from the moment it is
//! created. That is the whole reason to target the GC proposal rather than linear memory: a
//! hand-written collector would have to scan a stack it cannot see.
//!
//! # One module for the whole project
//!
//! Unlike the JVM backend, which emits one class file per declared type, this one takes every
//! source at once and emits a single module. wasm has no dynamic loading and no classpath: a call
//! from one type to another is a `call` to a function index, which only exists if both were
//! compiled together.
//!
//! # Scope
//!
//! Primitives, user-declared classes (fields, constructors, methods), and the control flow that
//! goes with them: arithmetic with Java's numeric promotions, the bitwise and shift operators,
//! comparisons at every width, reference identity and `instanceof`, casts, and the unary operators.
//! Library types are out of scope by design — there is no `java.base` on a wasm host, and supplying
//! one is a separate decision from compiling. So no `String`, no boxing, no exceptions, and no
//! interface dispatch: a call resolves to exactly one function, because the static type of the
//! receiver names exactly one method.
//!
//! # Where wasm and the JVM genuinely differ
//!
//! Most of the two backends' disagreements are spellings. Three are not:
//!
//! - **A shift's count.** `i64.shl` takes two `i64`s where `lshl` takes a `long` and an `int`, so the
//!   count is converted to the *result's* width here and left alone there.
//! - **Float-to-integer conversion traps.** `i32.trunc_f64_s` refuses a NaN, where JLS §5.1.3 wants a
//!   0 — so the saturating `trunc_sat` forms are the ones that mean what Java means.
//! - **There is no integer negation.** `-n` on an `int` is `0 - n`, which puts the zero on the stack
//!   *before* the operand rather than after it.

use alloc::borrow::ToOwned as _;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

use jals_hir::{DefId, DefKind, ItemId, MemberId, Primitive, ProjectIndex, Ty, TypedFile};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_BODY, ENUM_CONSTANT,
    ENUM_DECL, FIELD_DECL, INITIALIZER, INTERFACE_DECL, LAMBDA_EXPR, METHOD_DECL, METHOD_REF_EXPR,
    MODIFIERS, NEW_EXPR, RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::facts::{Facts, Hierarchy, Literal, Overrides};
use crate::wasm::encode::{
    CompType, ExportKind, FieldType, Func, Global, HeapType, Module, RefType, StorageType, SubType,
    ValType,
};
use crate::wasm::insn::{Insn, Num, NumOp};

/// Why a project could not be compiled to wasm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WasmError {
    /// A construct this backend does not emit.
    Unsupported(&'static str),
    /// A name the index did not resolve.
    Unresolved(String),
    /// A type with no wasm representation — every library type, by design.
    NoRepresentation(String),
    /// A length or an index outgrew the `u32` the binary format spells it with. Not reachable from
    /// a project that fits in memory, and reported rather than truncated because a wrong length is
    /// bytes an engine reads as something else.
    TooLarge,
}

impl core::fmt::Display for WasmError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "{what} is not compiled to wasm yet"),
            Self::Unresolved(name) => write!(f, "`{name}` did not resolve"),
            Self::NoRepresentation(ty) => write!(
                f,
                "`{ty}` has no wasm representation: this backend compiles primitives and \
                 project-declared classes, and a wasm host has no `java.base` to supply the rest"
            ),
            Self::TooLarge => f.write_str("the module exceeded a WebAssembly format limit"),
        }
    }
}

impl core::error::Error for WasmError {}

/// A source fact this backend could not be given. Both variants are ones `WasmError` already
/// spells, so the `&'static str` of an `Unsupported` reaches a caller verbatim — several are pinned
/// by name in the integration tests.
impl From<crate::facts::FactError> for WasmError {
    fn from(error: crate::facts::FactError) -> Self {
        match error {
            crate::facts::FactError::Unsupported(what) => Self::Unsupported(what),
            crate::facts::FactError::Unresolved(name) => Self::Unresolved(name),
        }
    }
}

type Result<T> = core::result::Result<T, WasmError>;

/// A method the module defines: where its body is and what it compiled to.
struct Method {
    /// The declaring class, or `None` for a `static` method that needs no receiver.
    owner: Option<ItemId>,
    /// The declaration node, for the second pass that lowers its body.
    node: SyntaxNode,
    /// Which input the node came from.
    input: usize,
    /// Index into the type section for the function's signature.
    signature: u32,
    /// Index in the function section.
    index: u32,
    /// The exported name, when this is a `public static` method.
    export: Option<String>,
    /// A constructor initialises `this` rather than returning a value.
    is_constructor: bool,
    /// The class this constructor's extra first parameter holds, when it belongs to an inner class.
    encloses: Option<ItemId>,
    /// How many trailing parameters hold captured locals.
    captures: usize,
    /// Whether this is the synthesised constructor of a class that declares none: its `node` is the class body
    /// and its only work is the initialisers.
    initialises: Option<ItemId>,
    /// The interface method this body implements, when the "method" is a lambda expression rather than a
    /// declaration. A lambda's captures are *fields*, not parameters, so only its own parameters are bound.
    lambda: Option<MemberId>,
    /// Whether the function's signature has a result, which decides whether its body needs a trailing
    /// `unreachable`.
    has_result: bool,
}

/// Compiles a whole project to one WebAssembly module.
pub struct CompileWasm;

impl CompileWasm {
    /// Emit the module. `index` must have been built over exactly `inputs`.
    pub fn project(inputs: &[TypedFile<'_>], index: &ProjectIndex) -> Result<Vec<u8>> {
        let mut module = Module::new();
        let mut layout = Layout::default();

        // Pass 1: every class *reserves* a struct type index, in an order where a supertype comes
        // first so its field prefix is known when the subtype is laid out. Only the index is fixed
        // here — the body waits, because a field of array type needs an array type index and an
        // array's element may be one of these classes.
        let mut interface_items = Vec::new();
        let mut inner_items = Vec::new();
        let mut captured_items = Vec::new();
        let mut body_nodes = Vec::new();
        let classes = Self::classes_in_order(
            inputs,
            index,
            &mut interface_items,
            &mut inner_items,
            &mut captured_items,
            &mut body_nodes,
        )?;
        for (item, node) in body_nodes {
            layout.bodies.insert(item, node);
        }
        for (item, enclosing) in inner_items {
            layout.inner.insert(item, enclosing);
        }
        for (item, captured) in captured_items {
            layout.captures.insert(item, captured);
        }
        // A subclass's own fields start after its supertype's, and an inner class's synthetic field sits
        // after *its* own — so a subclass would place its first field on top of it. Reported rather than
        // laid out wrong.
        for &item in &classes {
            if let Some(parent) = Self::superclass(item, index)
                && layout.inner.contains_key(&parent)
            {
                return Err(WasmError::Unsupported("a subclass of an inner class"));
            }
        }
        // An interface has no struct type, so it is registered before any class is laid out: a field or
        // a parameter of interface type has to resolve to *something* while the structs are built.
        for item in interface_items {
            layout.interfaces.insert(item);
        }
        // One tag, declared whether or not anything throws: an unused tag costs three bytes and saves
        // the lowering from having to know in advance whether a body will need one.
        let payload = module.add_type(SubType::plain(CompType::Func {
            params: alloc::vec![ValType::Ref(RefType::nullable(HeapType::Any))],
            results: Vec::new(),
        }));
        module.tags.push(payload);
        layout.tag = Some(u32::try_from(module.tags.len() - 1).map_err(|_| WasmError::TooLarge)?);
        for &item in &classes {
            layout.reserve_class(item, index, &mut module);
        }
        // Then every array type the program mentions. wasm has one declared type per element
        // type, and a body cannot introduce one mid-lowering, so they are collected from the
        // types the analyses already recorded rather than discovered while emitting.
        for input in inputs {
            for node in input.root().descendants() {
                if let Some(ty) = input.type_of_expr(Facts::span(&node)) {
                    layout.declare_array(ty, &mut module)?;
                }
            }
            for def in input.analysis().defs() {
                let ty = input.type_of_def(def.id).clone();
                layout.declare_array(&ty, &mut module)?;
            }
        }
        // Now every index is known, so the struct bodies can name array types and vice versa.
        for &item in &classes {
            layout.fill_class(item, index, &mut module)?;
        }
        // Then every `static` field, which is module state rather than a struct slot. After the
        // arrays, because a `static int[]` field's global needs its array type to exist.
        // `(field, initialiser)` for every `static` field whose value has to be *computed*, plus the
        // `static { … }` blocks, all of which run in the start function.
        let mut deferred = Vec::new();
        for input in inputs {
            layout.declare_statics(input, index, &mut module, &mut deferred)?;
        }
        // An `enum`'s constants are `static final` fields of the enum's own type, and the source writes
        // no initialiser for any of them: each one is an allocation, which is not a constant expression.
        // So they get globals here and are built in the start function, in declaration order.
        let mut constants = Vec::new();
        for input in inputs {
            layout.declare_constants(input, index, &mut module, &mut constants)?;
        }

        // Pass 2: every method gets a signature and a function index, so a call emitted in pass 3
        // can name a function declared later in the source.
        let mut methods = Vec::new();
        for (position, input) in inputs.iter().enumerate() {
            Self::collect_methods(
                input,
                position,
                index,
                &mut layout,
                &mut module,
                &mut methods,
            )?;
        }

        // A record's canonical constructor and its accessors have no declaration to walk: the header
        // declares the components and the compiler owes the rest. They are synthesised here, after every
        // *declared* method has its index, so the indices stay in step with the order bodies are pushed.
        let synthesised = Self::record_members(inputs, index, &mut layout, &mut module, &methods)?;

        // Each class's initialisation is a function of its own, reserved before any body so a body can
        // call it. A body has to: JLS §12.4.1 initialises a class on its first *use*, and this module's
        // one start function cannot express that ordering — a class declared later may be read by one
        // declared earlier, which is what left an `enum` constant built from a `static` field that was
        // still zero.
        let blocks: Vec<(usize, ItemId, ast::Block)> = inputs
            .iter()
            .enumerate()
            .flat_map(|(position, input)| {
                Self::static_initializers(input.root(), input, index)
                    .into_iter()
                    .map(move |(owner, block)| (position, owner, block))
            })
            .collect();
        let state = StaticState {
            deferred: &deferred,
            constants: &constants,
            blocks: &blocks,
        };
        Self::reserve_class_inits(
            inputs,
            index,
            &mut layout,
            &mut module,
            &state,
            methods.len() + synthesised.len(),
        );
        let inits = Self::class_initializers(inputs, index, &layout, &state, &mut module)?;

        // Pass 3: bodies.
        for method in &methods {
            let input = &inputs[method.input];
            let body = Body::lower(method, input, index, &layout)?;
            module.funcs.push(Func {
                type_index: method.signature,
                locals: body.locals,
                body: body.code,
            });
            if let Some(name) = &method.export {
                module
                    .exports
                    .push((name.clone(), ExportKind::Func, method.index));
            }
        }
        for func in synthesised {
            module.funcs.push(func);
        }
        for func in &inits {
            module.funcs.push(func.clone());
        }
        // Every class's initialisation, called in source order. A class whose initialisers read
        // another's have already run it by then — each one guards itself, so calling it again is free.
        if !inits.is_empty() {
            let mut insn = Insn::new();
            for &(function, _) in layout.class_inits.values() {
                insn.call(function);
            }
            let signature = module.add_type(SubType::plain(CompType::Func {
                params: Vec::new(),
                results: Vec::new(),
            }));
            let start = Module::func_index(module.funcs.len());
            module.funcs.push(Func {
                type_index: signature,
                locals: Vec::new(),
                body: insn.into_body(),
            });
            module.start = Some(start);
        }
        module.finish().ok_or(WasmError::TooLarge)
    }

    /// Give every class with static state a function index and a "has run" flag.
    ///
    /// Reserved before any body is lowered, because a body calls one: a `static` field read has to
    /// initialise the class that declares it first, and that class may be declared *after* the one
    /// reading it. The flag is what makes calling it again free, and what makes the re-entrant call a
    /// class's own initialiser produces a no-op rather than a loop — the same answer §12.4.2 gives.
    fn reserve_class_inits(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        state: &StaticState<'_>,
        mut next: usize,
    ) {
        let StaticState {
            deferred,
            constants,
            blocks,
        } = *state;
        // Source order, so a module with no cross-class dependency runs exactly what it used to.
        let mut owners = Vec::new();
        for (position, input) in inputs.iter().enumerate() {
            let mut push = |owner: ItemId| {
                if !owners.contains(&owner) {
                    owners.push(owner);
                }
            };
            for node in input.root().descendants() {
                if !Self::declares_a_type(&node) {
                    continue;
                }
                let Ok(owner) = Layout::owner_of(&node, input, index) else {
                    continue;
                };
                let has_constants = constants.iter().any(|&(_, item, _)| item == owner);
                let has_deferred = deferred
                    .iter()
                    .any(|(member, _)| index.member(*member).owner == owner);
                let has_blocks = blocks
                    .iter()
                    .any(|&(at, item, _)| at == position && item == owner);
                if has_constants || has_deferred || has_blocks {
                    push(owner);
                }
            }
        }
        for owner in owners {
            let mut init = Insn::new();
            init.i32_const(0);
            module.globals.push(Global {
                ty: ValType::I32,
                init: init.into_body(),
            });
            let flag = u32::try_from(module.globals.len() - 1).unwrap_or(0);
            layout
                .class_inits
                .insert(owner, (Module::func_index(next), flag));
            next += 1;
        }
    }

    /// One function per class with static state: its `enum` constants, then its computed `static`
    /// field initialisers and `static { … }` blocks, in source order (§8.9.3, §12.4.2).
    fn class_initializers(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &Layout,
        state: &StaticState<'_>,
        module: &mut Module,
    ) -> Result<Vec<Func>> {
        let StaticState {
            deferred,
            constants,
            blocks,
        } = *state;
        let signature = module.add_type(SubType::plain(CompType::Func {
            params: Vec::new(),
            results: Vec::new(),
        }));
        let mut out = Vec::new();
        for (&owner, &(_, flag)) in &layout.class_inits {
            let mut insn = Insn::new();
            // The guard, and the flag set *before* the body: a class whose initialiser reaches back to
            // its own statics gets the values written so far rather than a second run.
            insn.block();
            insn.global_get(flag);
            insn.br_if(0);
            insn.i32_const(1);
            insn.global_set(flag);
            let mut locals = Vec::new();
            for (position, input) in inputs.iter().enumerate() {
                let mut lowering = Lowering::for_static(input, index, layout, locals);
                // Every constant first: a `static { … }` block or a field initialiser may name one, and
                // §8.9.3 builds them before either runs.
                for (member, item, node) in constants {
                    if *item != owner || index.member(*member).file != input.file() {
                        continue;
                    }
                    let global = *layout
                        .statics
                        .get(member)
                        .ok_or(WasmError::Unsupported("an `enum` constant with no global"))?;
                    lowering.enum_constant(*item, node, &mut insn)?;
                    insn.global_set(global);
                }
                // A field initialiser and a `static { … }` block are one sequence in *source* order
                // (§12.4.2), not two: `static int a = 1; static { a = 2; } static int b = a;` leaves `b`
                // as 2, and running every field before every block left it as 1.
                let mut sequence: Vec<(usize, StaticStep<'_>)> = Vec::new();
                for entry in deferred {
                    let info = index.member(entry.0);
                    if info.owner == owner && info.file == input.file() {
                        sequence.push((info.name_range.start, StaticStep::Field(entry)));
                    }
                }
                for (at, item, block) in blocks {
                    if *at == position && *item == owner {
                        sequence.push((
                            usize::from(block.syntax().text_range().start()),
                            StaticStep::Block(block),
                        ));
                    }
                }
                sequence.sort_by_key(|&(at, _)| at);
                for (_, step) in sequence {
                    match step {
                        StaticStep::Field((member, value)) => {
                            let global = *layout
                                .statics
                                .get(member)
                                .ok_or(WasmError::Unsupported("a `static` field with no global"))?;
                            let declared = index.resolved_member_ty(*member);
                            lowering.assign_static(value, &declared, global, &mut insn)?;
                        }
                        StaticStep::Block(block) => lowering.block(block, &mut insn)?,
                    }
                }
                locals = lowering.locals;
            }
            insn.end();
            out.push(Func {
                type_index: signature,
                locals,
                body: insn.into_body(),
            });
        }
        Ok(out)
    }

    /// A record's canonical constructor and one accessor per component, written out rather than walked.
    ///
    /// A component is declared once, in the header, and stands for a field, an accessor, and a
    /// constructor parameter — none of which the body writes. The index already synthesises all three
    /// (that is what makes `r.x()` resolve), so what is missing here is only the code, and it is short
    /// enough to write directly: the constructor stores each parameter into its slot, and an accessor
    /// reads one back.
    ///
    /// `equals`, `hashCode`, and `toString` are *not* synthesised. All three come from
    /// `java.lang.Record` and two of them involve a `String`, which has no wasm representation by this
    /// backend's design — a call to one reports rather than being guessed at.
    fn record_members(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        methods: &[Method],
    ) -> Result<Vec<Func>> {
        let mut out = Vec::new();
        for input in inputs {
            for node in input.root().descendants() {
                if node.kind() != RECORD_DECL {
                    continue;
                }
                let owner = Layout::owner_of(&node, input, index)?;
                let struct_type = layout.structs[&owner];
                let this = layout.class_ref(owner)?;
                let components: Vec<MemberId> =
                    layout.fields.get(&owner).cloned().unwrap_or_default();

                // The canonical constructor, unless the body wrote one: `this` then one parameter per
                // component, each stored into its own slot.
                let declared_constructor = index.own_members(owner).iter().any(|&id| {
                    let m = index.member(id);
                    m.kind == DefKind::Constructor && m.name_range != (0..0)
                });
                if !declared_constructor
                    && let Some(&ctor) = index
                        .own_members(owner)
                        .iter()
                        .find(|&&id| index.member(id).kind == DefKind::Constructor)
                {
                    let mut params = alloc::vec![this];
                    let mut body = Insn::new();
                    for (position, &component) in components.iter().enumerate() {
                        let ty = layout.val_type(&index.resolved_member_ty(component))?;
                        params.push(ty);
                        let slot = u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?;
                        let field = layout
                            .field_slot(owner, component)
                            .ok_or(WasmError::Unsupported("a record component with no slot"))?;
                        body.local_get(0)
                            .local_get(slot)
                            .struct_set(struct_type, field);
                    }
                    let signature = module.add_type(SubType::plain(CompType::Func {
                        params,
                        results: Vec::new(),
                    }));
                    let function = Module::func_index(methods.len() + out.len());
                    layout.functions.insert(ctor, function);
                    out.push(Func {
                        type_index: signature,
                        locals: Vec::new(),
                        body: body.into_body(),
                    });
                }

                // One accessor per component, unless the body declared it by hand.
                for &component in &components {
                    let name = index.member(component).name.clone();
                    let accessor = index.own_members(owner).iter().copied().find(|&id| {
                        let m = index.member(id);
                        m.kind == DefKind::Method && m.name == name && m.params.is_empty()
                    });
                    let Some(accessor) = accessor else { continue };
                    if layout.functions.contains_key(&accessor) {
                        continue;
                    }
                    let ty = layout.val_type(&index.resolved_member_ty(component))?;
                    let field = layout
                        .field_slot(owner, component)
                        .ok_or(WasmError::Unsupported("a record component with no slot"))?;
                    let mut body = Insn::new();
                    body.local_get(0).struct_get(struct_type, field);
                    let signature = module.add_type(SubType::plain(CompType::Func {
                        params: alloc::vec![this],
                        results: alloc::vec![ty],
                    }));
                    let function = Module::func_index(methods.len() + out.len());
                    layout.functions.insert(accessor, function);
                    out.push(Func {
                        type_index: signature,
                        locals: Vec::new(),
                        body: body.into_body(),
                    });
                }
            }
        }
        Ok(out)
    }

    /// Every `static { … }` block in `root`, in source order, with the type that declares it.
    ///
    /// The owner is what groups a block with the field initialisers it runs beside: JLS §12.4.2 runs
    /// one class's static initialisers as one sequence, and a block that reads a field of *another*
    /// class is what makes the grouping observable.
    fn static_initializers(
        root: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Vec<(ItemId, ast::Block)> {
        let mut out = Vec::new();
        for node in root.descendants() {
            if node.kind() != INITIALIZER
                || !node
                    .children()
                    .filter(|child| child.kind() == MODIFIERS)
                    .flat_map(|modifiers| modifiers.children_with_tokens())
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|token| token.kind() == jals_syntax::SyntaxKind::STATIC_KW)
            {
                continue;
            }
            let owner = node
                .ancestors()
                .find(Self::declares_a_type)
                .and_then(|declaration| ast::Decl::name_token_of(&declaration))
                .and_then(|name| {
                    index.item_by_decl(input.file(), usize::from(name.text_range().start()))
                });
            if let (Some(owner), Some(block)) = (owner, node.children().find_map(ast::Block::cast))
            {
                out.push((owner, block));
            }
        }
        out
    }

    /// Whether a node is a type declaration, which is what a member's owner is found by walking to.
    fn declares_a_type(node: &SyntaxNode) -> bool {
        matches!(
            node.kind(),
            CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL
        )
    }

    /// Every project class, supertypes first.
    ///
    /// A struct's fields start with its supertype's, so the supertype's layout has to be settled
    /// first. The order is a depth-first walk of the `extends` chain; a cycle is impossible in a
    /// well-formed program and is simply not revisited here.
    fn classes_in_order(
        inputs: &[TypedFile<'_>],
        index: &ProjectIndex,
        interfaces: &mut Vec<ItemId>,
        inner: &mut Vec<(ItemId, ItemId)>,
        captures: &mut Vec<(ItemId, Vec<(DefId, Ty)>)>,
        bodies: &mut Vec<(ItemId, SyntaxNode)>,
    ) -> Result<Vec<ItemId>> {
        let mut declared = Vec::new();
        for input in inputs {
            for node in Self::type_declarations(input.root()) {
                // A type this backend does not lay out at all. Dropping one is what the class walk used
                // to do to *every* nested declaration: the type never exists, and the first use of it
                // reports an unresolved name that points at nothing a reader can act on.
                if let Some(what) = Self::unrepresentable_kind(node.kind()) {
                    return Err(WasmError::Unsupported(what));
                }
                let Some(item) = Self::item_of(&node, input, index)? else {
                    continue;
                };
                if node.kind() == INTERFACE_DECL {
                    interfaces.push(item);
                    continue;
                }
                // The set is the shared fact; the type beside each is this backend's, because the
                // layout wants a `ValType` where the JVM wants a descriptor. It is read from the
                // *declaring* input, which is the only place it is known — the layout is built long
                // before any body is lowered.
                let captured: Vec<(DefId, Ty)> = Facts::of(*input)
                    .captured_by(&node)
                    .into_iter()
                    .map(|id| (id, input.type_of_def(id).clone()))
                    .collect();
                if !captured.is_empty() {
                    captures.push((item, captured));
                }
                bodies.push((item, node.clone()));
                if Self::is_inner(&node) {
                    let enclosing = node.parent().and_then(|body| body.parent()).ok_or(
                        WasmError::Unsupported("an inner class with no enclosing type"),
                    )?;
                    inner.push((item, Layout::owner_of(&enclosing, input, index)?));
                }
                declared.push(item);
            }
        }

        let mut ordered: Vec<ItemId> = Vec::with_capacity(declared.len());
        for &item in &declared {
            Self::push_with_supertypes(item, index, &declared, &mut ordered);
        }
        Ok(ordered)
    }

    /// The declaration kinds this backend lays out no type for, each naming itself.
    ///
    /// An interface needs a dispatch mechanism (a function table or a per-type vtable struct), and an
    /// `enum` and a `record` need the synthesised members the JVM backend builds. None is laid out yet,
    /// and every one of them would otherwise vanish without a word.
    const fn unrepresentable_kind(kind: jals_syntax::SyntaxKind) -> Option<&'static str> {
        use jals_syntax::SyntaxKind::ANNOTATION_TYPE_DECL;
        match kind {
            ANNOTATION_TYPE_DECL => Some("an `@interface` declaration"),
            _ => None,
        }
    }

    /// Every type declaration in `root`, nested ones included.
    ///
    /// wasm's type space is flat and has no naming convention to satisfy, so a `static` nested class is
    /// simply another struct type — there is nothing for it to be nested *in*. Walking only the root's
    /// children dropped every one of them silently: the type never existed, and a call to one of its
    /// methods reported an unresolved name that pointed nowhere useful.
    ///
    /// A class inside a *block* is a local class, and wasm's flat type space has nothing to say about
    /// where it was written — so it is laid out like any other. What it may *not* do is capture a local:
    /// each capture needs a synthetic constructor parameter, and `captures_a_local` reports one.
    fn type_declarations(root: &SyntaxNode) -> impl Iterator<Item = SyntaxNode> + '_ {
        use jals_syntax::SyntaxKind::{
            ANNOTATION_TYPE_DECL, ENUM_DECL, INTERFACE_DECL, RECORD_DECL,
        };
        root.descendants().filter(|node| {
            matches!(
                node.kind(),
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL
            ) || Self::is_anonymous(node)
                || Self::is_functional(node)
        })
    }

    /// Whether `node` is an anonymous class body: a `new` with a class body of its own. It is a type
    /// declaration with no name and no keyword, so it is recognised by shape; the index keys its item on
    /// the `new` keyword's position, which is the only offset it can be found by.
    fn is_anonymous(node: &SyntaxNode) -> bool {
        // An `enum` constant with a body is the other form: an anonymous subclass of the enum, keyed on
        // the constant's own position for the same reason — there is no name to key on.
        matches!(node.kind(), NEW_EXPR | ENUM_CONSTANT)
            && node.children().any(|child| child.kind() == CLASS_BODY)
    }

    /// Whether `node` is a lambda or a method reference — the two forms the index gives a one-method class
    /// item to, and which a backend with no `invokedynamic` emits as exactly that.
    fn is_functional(node: &SyntaxNode) -> bool {
        matches!(node.kind(), LAMBDA_EXPR | METHOD_REF_EXPR)
    }

    /// The item a type declaration declares, whether it has a name to look up or only a position.
    fn item_of(
        node: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Result<Option<ItemId>> {
        // A lambda and an anonymous body are both nameless, and the index keys each on its own start
        // offset — the only thing either has to be found by.
        if Self::is_anonymous(node) || Self::is_functional(node) {
            return Ok(index.item_by_decl(input.file(), usize::from(node.text_range().start())));
        }
        let name =
            ast::Decl::name_token_of(node).ok_or(WasmError::Unsupported("a class with no name"))?;
        index
            .item_by_decl(input.file(), usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))
            .map(Some)
    }

    /// Whether a class declaration is a non-`static` nested one.
    fn is_inner(node: &SyntaxNode) -> bool {
        // A nested interface, `enum`, `record`, and `@interface` are all implicitly `static` and hold no
        // enclosing instance, so only a nested *class* can be an inner one.
        if node.kind() != CLASS_DECL {
            return false;
        }
        let nested = node
            .parent()
            .is_some_and(|parent| parent.kind() == jals_syntax::SyntaxKind::CLASS_BODY);
        nested
            && !node
                .children()
                .filter(|child| child.kind() == MODIFIERS)
                .flat_map(|modifiers| modifiers.children_with_tokens())
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .any(|token| token.kind() == jals_syntax::SyntaxKind::STATIC_KW)
    }

    fn push_with_supertypes(
        item: ItemId,
        index: &ProjectIndex,
        declared: &[ItemId],
        ordered: &mut Vec<ItemId>,
    ) {
        if ordered.contains(&item) {
            return;
        }
        if let Some(parent) = Self::superclass(item, index)
            && declared.contains(&parent)
        {
            Self::push_with_supertypes(parent, index, declared, ordered);
        }
        ordered.push(item);
    }

    /// The class a type extends, when that class is itself indexed.
    fn superclass(item: ItemId, index: &ProjectIndex) -> Option<ItemId> {
        index
            .item(item)
            .supertypes
            .iter()
            .map(|supertype| supertype.id)
            // An `enum` counts: a constant with a body is a subclass of one, and it is the only way a
            // declaration that is not a `class` ever appears here. Its struct has to hold the enum's
            // fields, which is what makes the layout inherit and the subtyping declared.
            .find(|&id| {
                matches!(
                    index.item(id).kind,
                    DefKind::Class | DefKind::Enum | DefKind::Record
                )
            })
    }

    /// Register every method and constructor `input` declares.
    fn collect_methods(
        input: &TypedFile<'_>,
        position: usize,
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        out: &mut Vec<Method>,
    ) -> Result<()> {
        for class in Self::type_declarations(input.root()) {
            let Some(item) = Self::item_of(&class, input, index)? else {
                continue;
            };
            // A lambda has no body *node* of members: it declares exactly one method, the interface's, and
            // the lambda expression itself is that method's body.
            if Self::is_functional(&class) {
                let Some(member) = index
                    .own_members(item)
                    .iter()
                    .copied()
                    .find(|&id| index.member(id).kind == DefKind::Method)
                else {
                    continue;
                };
                let mut params = alloc::vec![layout.class_ref(item)?];
                for ty in index.resolved_param_tys(member) {
                    params.push(layout.val_type(&ty)?);
                }
                let results = match index.resolved_member_ty(member) {
                    Ty::Void => Vec::new(),
                    ty => alloc::vec![layout.val_type(&ty)?],
                };
                let has_result = !results.is_empty();
                let signature = module.add_type(SubType::plain(CompType::Func { params, results }));
                let function = Module::func_index(out.len());
                layout.functions.insert(member, function);
                out.push(Method {
                    owner: Some(item),
                    node: class.clone(),
                    input: position,
                    signature,
                    index: function,
                    export: None,
                    is_constructor: false,
                    has_result,
                    encloses: None,
                    captures: 0,
                    initialises: None,
                    lambda: Some(member),
                });
                continue;
            }
            // An `enum`'s members live under an `ENUM_BODY`, after the constants and the `;`.
            let Some(body) = class
                .children()
                .find(|child| matches!(child.kind(), CLASS_BODY | ENUM_BODY))
            else {
                continue;
            };
            // A class that declares no constructor still has initialisers to run, and an initialiser *block*
            // reads its own fields through `this` — which only a function has a slot 0 to be. So one is
            // synthesised, and the `new` calls it.
            let declares_constructor = body
                .children()
                .any(|member| member.kind() == CONSTRUCTOR_DECL);
            let has_initialisers = body.children().any(|member| {
                member.kind() == INITIALIZER
                    || (member.kind() == FIELD_DECL
                        && member
                            .children()
                            .any(|child| ast::Expr::cast(child).is_some()))
            });
            // An interface has no instances, so it has no instance initialisers: its fields are
            // implicitly `static` (§9.3) and run in the class's initialisation, not a constructor's.
            if !declares_constructor && has_initialisers && class.kind() != INTERFACE_DECL {
                let signature = module.add_type(SubType::plain(CompType::Func {
                    params: alloc::vec![layout.class_ref(item)?],
                    results: Vec::new(),
                }));
                let function = Module::func_index(out.len());
                layout.default_constructors.insert(item, function);
                out.push(Method {
                    owner: Some(item),
                    node: body.clone(),
                    input: position,
                    signature,
                    index: function,
                    export: None,
                    is_constructor: false,
                    has_result: false,
                    encloses: None,
                    captures: 0,
                    initialises: Some(item),
                    lambda: None,
                });
            }
            for node in body.children() {
                if !matches!(node.kind(), METHOD_DECL | CONSTRUCTOR_DECL) {
                    continue;
                }
                // An abstract method has no body, so there is no function to declare: a virtual call
                // reaches the *implementations* instead. Declaring one would put a signature with a
                // result type over an empty body, which no engine accepts.
                let is_constructor = node.kind() == CONSTRUCTOR_DECL;
                if !is_constructor && node.children().find_map(ast::Block::cast).is_none() {
                    continue;
                }
                let member_name = Self::member_name_token(&node, is_constructor)
                    .ok_or(WasmError::Unsupported("a member with no name"))?;
                let member = index
                    .member_by_decl(input.file(), usize::from(member_name.text_range().start()))
                    .ok_or_else(|| WasmError::Unresolved(member_name.text().into()))?;
                let is_static = index.member(member).modifiers.is_static;

                let mut params = Vec::new();
                // An inner class's constructor takes the enclosing instance right after `this`, and
                // stores it into the synthetic field before the body runs.
                let encloses = is_constructor
                    .then(|| layout.inner.get(&item).copied())
                    .flatten();
                if is_constructor || !is_static {
                    params.push(layout.class_ref(item)?);
                }
                if let Some(enclosing) = encloses {
                    params.push(layout.class_ref(enclosing)?);
                }
                for ty in index.resolved_param_tys(member) {
                    params.push(layout.val_type(&ty)?);
                }
                // The captures come after every declared parameter, so a declared one keeps its slot.
                let captured = is_constructor
                    .then(|| layout.captures.get(&item).cloned())
                    .flatten()
                    .unwrap_or_default();
                for (_, ty) in &captured {
                    params.push(layout.val_type(ty)?);
                }
                let results = if is_constructor {
                    Vec::new()
                } else {
                    match index.resolved_member_ty(member) {
                        Ty::Void => Vec::new(),
                        ty => alloc::vec![layout.val_type(&ty)?],
                    }
                };

                let has_result = !results.is_empty();
                let signature = module.add_type(SubType::plain(CompType::Func { params, results }));
                let function = Module::func_index(out.len());
                layout.functions.insert(member, function);
                out.push(Method {
                    owner: (!is_static).then_some(item),
                    node: node.clone(),
                    input: position,
                    signature,
                    index: function,
                    // A `public static` method is the module's surface: a wasm host has no `main`
                    // convention, so every one of them is exported by name.
                    has_result,
                    encloses,
                    captures: captured.len(),
                    initialises: None,
                    lambda: None,
                    export: (is_static && !is_constructor)
                        .then(|| index.member(member).name.clone()),
                    is_constructor,
                });
            }
        }
        Ok(())
    }

    /// The name token of a class member, which is a method or a constructor here.
    ///
    /// Two kinds rather than one because `ConstructorDecl` is not a [`ast::Decl`] variant — a
    /// constructor declares no type and no field, so the grammar keeps it out of that enum. Both
    /// arms go through the node's own generated accessor.
    fn member_name_token(node: &SyntaxNode, is_constructor: bool) -> Option<SyntaxToken> {
        if is_constructor {
            ast::ConstructorDecl::cast(node.clone())?.name_token()
        } else {
            ast::MethodDecl::cast(node.clone())?.name_token()
        }
    }
}

/// How Java declarations map onto wasm types and indices.
#[derive(Default)]
struct Layout {
    /// Each class's struct type index.
    structs: BTreeMap<ItemId, u32>,
    /// Each class's instance fields, in slot order, including those inherited.
    fields: BTreeMap<ItemId, Vec<MemberId>>,
    /// Each method's function index.
    functions: BTreeMap<MemberId, u32>,
    /// A non-`static` nested class and the class that encloses it. Its instance holds the enclosing one
    /// in a synthetic field, appended *after* its own — which keeps every real field's slot where
    /// `field_slot` computes it, and is why a class extending an inner class is reported instead.
    inner: BTreeMap<ItemId, ItemId>,
    /// The synthetic enclosing-instance field's index, for each inner class.
    outer: BTreeMap<ItemId, u32>,
    /// The function that runs a constructor-less class's initialisers, for the `new` to call. A block reads its
    /// own fields through `this`, and only a function has a slot 0 to be `this`.
    default_constructors: BTreeMap<ItemId, u32>,
    /// Each class's declaration node, so a `new` can reach the field initialisers of a class *other* than the
    /// one being lowered — which is the only way a class with no constructor ever runs them.
    bodies: BTreeMap<ItemId, SyntaxNode>,
    /// Every local class and the locals it captures, in source order. Each becomes a struct field and a
    /// *trailing* constructor parameter, which is how the class outlives the frame the local lived in.
    captures: BTreeMap<ItemId, Vec<(DefId, Ty)>>,
    /// The struct field index of the first capture, for each class that has any.
    capture_slot: BTreeMap<ItemId, u32>,
    /// The one exception tag, if the module throws anything. Every Java throw is a reference, so one
    /// tag carrying one reference covers all of them and the *class* of that reference is what a
    /// `catch` tests.
    tag: Option<u32>,
    /// Every interface this module declares. An interface gets no struct type — wasm's declared
    /// subtyping is single-inheritance, so it could not be a supertype of two unrelated classes — so a
    /// value of interface type is held at the top of the reference hierarchy and narrowed at each use.
    interfaces: BTreeSet<ItemId>,
    /// Each `static` field's global index. A Java `static` field is module state, which is what a
    /// wasm global is; an instance field is a struct slot instead.
    statics: BTreeMap<MemberId, u32>,
    /// `(initialiser function, "has run" flag global)` for each class with static state.
    ///
    /// A JVM initialises a class on its first *use* (JLS §12.4.1), and one start function cannot
    /// express that: a class declared later may be read by one declared earlier. So each class's
    /// initialisation is its own guarded function, called from the start function in source order and
    /// again from every `static` access — the guard makes all but the first call free.
    class_inits: BTreeMap<ItemId, (u32, u32)>,
    /// `(element type, array type index)`. A `Vec` because `ValType` has no ordering and a program
    /// has a handful of distinct element types.
    arrays: Vec<(ValType, u32)>,
}

impl Layout {
    /// Reserve `item`'s struct type index and work out which fields it holds.
    ///
    /// Reserving rather than declaring is what makes an array-typed field work: the field's *type*
    /// needs an array type index, and an array's element may be a class — so neither can be laid out
    /// before the other. Every type lives in one recursive group, so an index may be referred to
    /// before its body exists; [`fill_class`](Self::fill_class) writes the body once every index is
    /// known.
    ///
    /// The field list itself needs no types, only its supertype's list first — which is why
    /// `classes_in_order` still walks supertypes first.
    fn reserve_class(&mut self, item: ItemId, index: &ProjectIndex, module: &mut Module) {
        if self.structs.contains_key(&item) {
            return;
        }
        let parent =
            CompileWasm::superclass(item, index).filter(|id| self.structs.contains_key(id));
        let mut members: Vec<MemberId> = parent
            .and_then(|id| self.fields.get(&id))
            .cloned()
            .unwrap_or_default();
        for &member in index.own_members(item) {
            let info = index.member(member);
            if info.kind == DefKind::Field && !info.modifiers.is_static {
                members.push(member);
            }
        }
        self.structs.insert(item, module.reserve_type());
        self.fields.insert(item, members);
    }

    /// Write `item`'s struct body: its supertype's fields followed by its own, at their wasm types.
    fn fill_class(
        &mut self,
        item: ItemId,
        index: &ProjectIndex,
        module: &mut Module,
    ) -> Result<()> {
        let Some(&type_index) = self.structs.get(&item) else {
            return Ok(());
        };
        let members = self.fields.get(&item).cloned().unwrap_or_default();
        let mut fields = Vec::with_capacity(members.len());
        for &member in &members {
            fields.push(FieldType {
                storage: StorageType::Val(self.val_type(&index.resolved_member_ty(member))?),
                // Every Java field is assignable unless `final`, and even a `final` one is written
                // once by a constructor — after `struct.new_default` has already made it.
                mutable: true,
            });
        }
        // The enclosing instance goes last, so every real field keeps the slot `field_slot` computes.
        let outer = self.inner.get(&item).copied();
        if let Some(enclosing) = outer {
            let slot = u32::try_from(fields.len()).map_err(|_| WasmError::TooLarge)?;
            fields.push(FieldType {
                storage: StorageType::Val(self.class_ref(enclosing)?),
                mutable: true,
            });
            self.outer.insert(item, slot);
        }
        // One field per captured local, after the enclosing instance: the class outlives the frame the
        // local lived in, so the value has to be copied into it.
        let captured = self.captures.get(&item).cloned().unwrap_or_default();
        if !captured.is_empty() {
            let first = u32::try_from(fields.len()).map_err(|_| WasmError::TooLarge)?;
            self.capture_slot.insert(item, first);
            for (_, ty) in &captured {
                fields.push(FieldType {
                    storage: StorageType::Val(self.val_type(ty)?),
                    mutable: true,
                });
            }
        }
        let parent =
            CompileWasm::superclass(item, index).and_then(|id| self.structs.get(&id).copied());
        module.set_type(
            type_index,
            SubType {
                is_final: false,
                supertype: parent,
                comp: CompType::Struct(fields),
            },
        );
        Ok(())
    }

    /// Give every `static` field a mutable global, initialised in place.
    ///
    /// A global's initialiser is a *constant expression*: the format allows only a handful of
    /// instructions there, so anything a `<clinit>` would have to compute cannot live in one. A
    /// non-constant initialiser is reported rather than replaced by the type's default — silently
    /// dropping a `static` initialiser is a wrong value in a module that validates.
    fn declare_statics(
        &mut self,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        module: &mut Module,
        out: &mut Vec<(MemberId, ast::Expr)>,
    ) -> Result<()> {
        for node in input.root().descendants() {
            if node.kind() != FIELD_DECL {
                continue;
            }
            let Some(declaration) = ast::FieldDecl::cast(node.clone()) else {
                continue;
            };
            let names: Vec<SyntaxToken> = declaration.names().collect();
            let values: Vec<ast::Expr> = node.children().filter_map(ast::Expr::cast).collect();
            for (position, name) in names.iter().enumerate() {
                let Some(member) =
                    index.member_by_decl(input.file(), usize::from(name.text_range().start()))
                else {
                    continue;
                };
                if !index.member(member).modifiers.is_static {
                    continue;
                }
                // A field whose type has no wasm representation gets no global, and no report either:
                // an unrepresentable type is reported where it is *used*, so a generated
                // `static final String` nothing reads stays inert — which is what keeps a project
                // compiling for wasm alongside a class the user never wrote.
                let Ok(ty) = self.val_type(&index.resolved_member_ty(member)) else {
                    continue;
                };
                let (init, deferred) = Self::constant_init(values.get(position), ty);
                module.globals.push(Global { ty, init });
                let global =
                    u32::try_from(module.globals.len() - 1).map_err(|_| WasmError::TooLarge)?;
                self.statics.insert(member, global);
                // A global's own initialiser is a constant expression, so anything that has to be
                // *computed* runs in the start function instead — over the global the default already
                // holds, which is exactly the order a `<clinit>` gives.
                if deferred && let Some(value) = values.get(position) {
                    out.push((member, value.clone()));
                }
            }
        }
        Ok(())
    }

    /// Give every `enum` constant a global of its enum's own type.
    ///
    /// A constant is a `static final` field whose value the source never writes: it is an allocation,
    /// which no constant expression can hold, so the global starts as `null` and the start function
    /// builds it. A constant with a **body** is *reported* — it is an anonymous subclass, which is its
    /// own type.
    ///
    /// The two synthetic parameters a JVM `enum` constructor takes (`name`, `ordinal`) have nothing to
    /// carry here: `name()` and `ordinal()` come from `java.lang.Enum` and involve a `String`, which
    /// this backend has no representation for. So a constant's arguments go straight to the declared
    /// constructor, with nothing ahead of them.
    fn declare_constants(
        &mut self,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        module: &mut Module,
        out: &mut Vec<(MemberId, ItemId, SyntaxNode)>,
    ) -> Result<()> {
        for node in input.root().descendants() {
            if node.kind() != ENUM_DECL {
                continue;
            }
            let owner = Self::owner_of(&node, input, index)?;
            let body = node.children().find(|child| child.kind() == ENUM_BODY);
            for member in body.iter().flat_map(SyntaxNode::children) {
                let Some(constant) = ast::EnumConstant::cast(member.clone()) else {
                    continue;
                };
                let name = constant
                    .name_token()
                    .ok_or(WasmError::Unsupported("an `enum` constant with no name"))?;
                let id = index
                    .member_by_decl(input.file(), usize::from(name.text_range().start()))
                    .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
                let ty = self.class_ref(owner)?;
                let mut init = Insn::new();
                Self::default_value(ty, &mut init);
                module.globals.push(Global {
                    ty,
                    init: init.into_body(),
                });
                let global =
                    u32::try_from(module.globals.len() - 1).map_err(|_| WasmError::TooLarge)?;
                self.statics.insert(id, global);
                out.push((id, owner, member.clone()));
            }
        }
        Ok(())
    }

    /// The indexed item a type declaration declares.
    fn owner_of(node: &SyntaxNode, input: &TypedFile<'_>, index: &ProjectIndex) -> Result<ItemId> {
        let name = ast::Decl::name_token_of(node)
            .ok_or(WasmError::Unsupported("a type declaration with no name"))?;
        index
            .item_by_decl(input.file(), usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))
    }

    /// The constant expression a `static` field's global is initialised with.
    ///
    /// No initialiser is the type's default, which is exactly Java's rule (§4.12.5). A literal folds
    /// into the same shape. Anything else — including a literal that would need a widening conversion,
    /// since a constant expression cannot hold one — is reported rather than replaced by the default.
    fn constant_init(value: Option<&ast::Expr>, ty: ValType) -> (Vec<u8>, bool) {
        use jals_syntax::SyntaxKind::{
            CHAR_LITERAL, FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, TRUE_KW,
        };
        // Anything a constant expression cannot hold falls back to the type's default *and* asks for a
        // start-function assignment, which is the order a `<clinit>` gives: the field holds its default
        // until the initialiser runs.
        let default = || {
            let mut insn = Insn::new();
            Self::default_value(ty, &mut insn);
            (insn.into_body(), true)
        };
        let Some(value) = value else {
            let mut insn = Insn::new();
            Self::default_value(ty, &mut insn);
            return (insn.into_body(), false);
        };
        let ast::Expr::Literal(literal) = value else {
            return default();
        };
        let Some(token) = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
        else {
            return default();
        };
        let text = token.text();
        let mut insn = Insn::new();
        match (token.kind(), ty) {
            (NULL_KW, ValType::Ref(_)) => {
                insn.ref_null(HeapType::None);
            }
            (TRUE_KW, ValType::I32) => {
                insn.i32_const(1);
            }
            (FALSE_KW, ValType::I32) => {
                insn.i32_const(0);
            }
            (CHAR_LITERAL, ValType::I32) => {
                let Ok(character) = Literal::character(text) else {
                    return default();
                };
                insn.i32_const(character as i32);
            }
            // An `int` literal into a wider field is an assignment conversion, and the *constant* form
            // of one is folding: `static long n = 1` writes `i64.const 1`, not `i32.const` plus an
            // extension no constant expression may hold. Which width to fold *into* is the field's,
            // so the one the fact reads off the suffix is dropped.
            (INT_LITERAL, _) => {
                let Ok((value, _)) = Literal::integer(text) else {
                    return default();
                };
                #[allow(clippy::cast_precision_loss)]
                match ty {
                    ValType::I64 => insn.i64_const(value),
                    ValType::F32 => insn.f32_const(value as f32),
                    ValType::F64 => insn.f64_const(value as f64),
                    _ => match i32::try_from(value) {
                        Ok(value) => insn.i32_const(value),
                        Err(_) => return default(),
                    },
                };
            }
            (FLOAT_LITERAL, ValType::F32 | ValType::F64) => {
                let Ok((value, _)) = Literal::floating(text) else {
                    return default();
                };
                #[allow(clippy::cast_possible_truncation)]
                if ty == ValType::F32 {
                    insn.f32_const(value as f32);
                } else {
                    insn.f64_const(value);
                }
            }
            // A literal whose type is not the field's and not foldable into it — the start function
            // lowers it with the conversion the expression path already knows how to emit.
            _ => return default(),
        }
        (insn.into_body(), false)
    }

    /// A type's default value: what a `static` field with no initialiser holds (§4.12.5).
    fn default_value(ty: ValType, insn: &mut Insn) {
        match ty {
            ValType::I32 => insn.i32_const(0),
            ValType::I64 => insn.i64_const(0),
            ValType::F32 => insn.f32_const(0.0),
            ValType::F64 => insn.f64_const(0.0),
            ValType::Ref(_) => insn.ref_null(HeapType::None),
        };
    }

    /// Declare the array type `ty` needs, and any nested one inside it (`int[][]` is an array of
    /// arrays, so the inner type has to exist first).
    fn declare_array(&mut self, ty: &Ty, module: &mut Module) -> Result<()> {
        let Ty::Array(element) = ty else {
            return Ok(());
        };
        self.declare_array(element, module)?;
        // A type this backend cannot represent is not an error *here*: it only matters if
        // something actually uses it, and that use reports it with the right span.
        let Ok(element) = self.val_type(element) else {
            return Ok(());
        };
        if self.array_type(element).is_some() {
            return Ok(());
        }
        let type_index = module.add_type(SubType::plain(CompType::Array(FieldType {
            storage: StorageType::Val(element),
            mutable: true,
        })));
        self.arrays.push((element, type_index));
        Ok(())
    }

    /// The declared array type whose elements are `element`.
    fn array_type(&self, element: ValType) -> Option<u32> {
        self.arrays
            .iter()
            .find(|(candidate, _)| *candidate == element)
            .map(|(_, index)| *index)
    }

    /// A nullable reference to `item`'s struct type — how every Java reference is represented.
    fn class_ref(&self, item: ItemId) -> Result<ValType> {
        if self.interfaces.contains(&item) {
            return Ok(ValType::Ref(RefType::nullable(HeapType::Any)));
        }
        let index = self
            .structs
            .get(&item)
            .ok_or_else(|| WasmError::NoRepresentation("an undeclared class".to_owned()))?;
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(*index))))
    }

    /// The wasm type a Java value of type `ty` has.
    fn val_type(&self, ty: &Ty) -> Result<ValType> {
        Ok(match ty {
            // Every integral type narrower than `long` computes as `i32`, exactly as on the JVM.
            Ty::Primitive(
                Primitive::Boolean
                | Primitive::Byte
                | Primitive::Short
                | Primitive::Char
                | Primitive::Int,
            ) => ValType::I32,
            Ty::Primitive(Primitive::Long) => ValType::I64,
            Ty::Primitive(Primitive::Float) => ValType::F32,
            Ty::Primitive(Primitive::Double) => ValType::F64,
            Ty::Class(_) => {
                let item = ty
                    .project_id()
                    .filter(|id| self.structs.contains_key(id) || self.interfaces.contains(id))
                    .ok_or_else(|| WasmError::NoRepresentation(ty.to_string()))?;
                self.class_ref(item)?
            }
            Ty::Array(element) => {
                let element = self.val_type(element)?;
                let array = self
                    .array_type(element)
                    .ok_or_else(|| WasmError::NoRepresentation(ty.to_string()))?;
                ValType::Ref(RefType::nullable(HeapType::Concrete(array)))
            }
            other => return Err(WasmError::NoRepresentation(other.to_string())),
        })
    }

    fn field_slot(&self, owner: ItemId, member: MemberId) -> Option<u32> {
        let slot = self.fields.get(&owner)?.iter().position(|&f| f == member)?;
        u32::try_from(slot).ok()
    }
}

/// Everything that runs in some class's initialisation, before it is grouped by the class it belongs to.
#[derive(Clone, Copy)]
struct StaticState<'a> {
    /// `(field, initialiser)` for every `static` field whose value has to be computed.
    deferred: &'a [(MemberId, ast::Expr)],
    /// `(field, enum, declaration)` for every `enum` constant, which is an allocation rather than a
    /// constant expression.
    constants: &'a [(MemberId, ItemId, SyntaxNode)],
    /// `(input, owner, block)` for every `static { … }`.
    blocks: &'a [(usize, ItemId, ast::Block)],
}

/// One step of a class's static sequence, which JLS §12.4.2 runs in *source* order.
enum StaticStep<'a> {
    /// A `static` field's computed initialiser.
    Field(&'a (MemberId, ast::Expr)),
    /// A `static { … }` block.
    Block(&'a ast::Block),
}

/// One method body being lowered.
struct Body {
    locals: Vec<ValType>,
    code: Vec<u8>,
}

impl Body {
    fn lower(
        method: &Method,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
        layout: &Layout,
    ) -> Result<Self> {
        let mut lowering = Lowering {
            input,
            index,
            layout,
            slots: Vec::new(),
            locals: Vec::new(),
            next: 0,
            owner: method.owner,
            loops: Vec::new(),
            pending_label: None,
            cleanups: Vec::new(),
            yields: Vec::new(),
        };
        // `this` is parameter 0 of an instance method or a constructor.
        if method.owner.is_some() || method.is_constructor {
            lowering.next += 1;
        }
        // An inner class's constructor takes the enclosing instance next, before any declared parameter.
        if method.encloses.is_some() {
            lowering.next += 1;
        }

        // A lambda's parameters live under its own `LambdaParams`, and its captures are fields rather than
        // parameters — so nothing trails them.
        // The synthesised constructor: `this` is slot 0, so a block initialiser reads its fields the way every
        // other body does.
        if let Some(owner) = method.initialises {
            let mut insn = Insn::new();
            // The implicit `super()` first, as a declared constructor's is: the superclass's
            // initialisers run before this class's (§12.5). Except under an `enum`: a constant's body is
            // a subclass whose *constant site* calls the enum's constructor, that being the one place
            // the constant's arguments exist — calling it here too would run the enum's twice, and the
            // no-argument one at that, which is a different constructor from the one selected.
            let under_enum = CompileWasm::superclass(owner, index)
                .is_some_and(|parent| index.item(parent).kind == DefKind::Enum);
            if let Some(function) = Self::super_constructor(owner, index, layout)
                && !under_enum
            {
                insn.local_get(0).call(function);
            }
            lowering.initializers(owner, &method.node, 0, &mut insn)?;
            return Ok(Self {
                locals: lowering.locals,
                code: insn.into_body(),
            });
        }
        if let Some(member) = method.lambda
            && method.node.kind() == METHOD_REF_EXPR
        {
            // A method reference's body is one delegation: pass the interface method's own arguments straight
            // to the method the source named. Its parameters need no bindings, because nothing reads them by
            // name — they are forwarded by position.
            let mut insn = Insn::new();
            // `T::new` allocates rather than delegating: the object *is* what the interface method returns.
            if Lowering::constructs(&method.node) {
                let created = Lowering::constructed_item(&method.node, input, index)?;
                let struct_type = layout.structs[&created];
                insn.struct_new_default(struct_type);
                let arity = index.member(member).params.len();
                let constructor = index.own_members(created).iter().copied().find(|&id| {
                    let info = index.member(id);
                    info.kind == DefKind::Constructor && info.params.len() == arity
                });
                if let Some(constructor) = constructor {
                    let function =
                        *layout
                            .functions
                            .get(&constructor)
                            .ok_or(WasmError::Unsupported(
                                "a constructor reference to a constructor with no body",
                            ))?;
                    let slot = u32::try_from(lowering.locals.len()).unwrap_or(0) + lowering.next;
                    lowering.locals.push(layout.class_ref(created)?);
                    insn.local_set(slot).local_get(slot);
                    for position in 0..arity {
                        insn.local_get(
                            u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?,
                        );
                    }
                    insn.call(function).local_get(slot);
                } else if arity > 0 {
                    return Err(WasmError::Unsupported(
                        "a constructor reference with no matching constructor",
                    ));
                } else if let Some(initialise) = layout
                    .default_constructors
                    .get(&created)
                    .copied()
                    .or_else(|| Self::super_constructor(created, index, layout))
                {
                    // Declaring no constructor does not mean there is nothing to run: the synthesised one
                    // runs the field initialisers, and it is the same function a plain `new` calls.
                    let slot = u32::try_from(lowering.locals.len()).unwrap_or(0) + lowering.next;
                    lowering.locals.push(layout.class_ref(created)?);
                    insn.local_set(slot).local_get(slot).call(initialise);
                    insn.local_get(slot);
                }
                insn.return_();
                return Ok(Self {
                    locals: lowering.locals,
                    code: insn.into_body(),
                });
            }
            let reference = Facts::of(*input).method_ref(&method.node)?;
            // This backend lowers a plain delegation only: a bound reference captures its receiver
            // and a constructor one needs an allocation, and neither is one.
            let target = reference.target.ok_or(WasmError::Unsupported(
                "a method reference to a constructor",
            ))?;
            let bound = matches!(reference.receiver, crate::facts::RefReceiver::Bound(_));
            let function = *layout.functions.get(&target).ok_or(WasmError::Unsupported(
                "a method reference to a method outside this module",
            ))?;
            let arity = index.member(member).params.len();
            // A *bound* reference's receiver was captured when the object was built, so it comes out of the
            // field rather than off the argument list — and it has to go on first, being the receiver.
            if bound {
                let owner = method
                    .owner
                    .ok_or(WasmError::Unsupported("a bound reference with no owner"))?;
                let field = *layout
                    .capture_slot
                    .get(&owner)
                    .ok_or(WasmError::Unsupported("a bound reference with no capture"))?;
                insn.local_get(0).struct_get(layout.structs[&owner], field);
            }
            // A `static` target takes the arguments alone; an unbound instance one takes the first as its
            // receiver, and forwarding by position already puts it there.
            for position in 0..arity {
                insn.local_get(u32::try_from(position + 1).map_err(|_| WasmError::TooLarge)?);
            }
            insn.call(function).return_();
            return Ok(Self {
                locals: Vec::new(),
                code: insn.into_body(),
            });
        }
        if let Some(member) = method.lambda {
            for param in method
                .node
                .descendants()
                .filter(|node| node.kind() == jals_syntax::SyntaxKind::PARAM)
            {
                let id = lowering
                    .facts()
                    .def_at(&param)
                    .ok_or(WasmError::Unsupported("a lambda parameter with no binding"))?;
                let ty = lowering.layout.val_type(lowering.input.type_of_def(id))?;
                lowering.slots.push((id, lowering.next));
                lowering.next += 1;
                let _ = ty;
            }
            let mut insn = Insn::new();
            let returns = index.resolved_member_ty(member);
            let body = method.node.children().find_map(ast::Block::cast);
            match (
                method
                    .node
                    .children()
                    .filter_map(ast::Expr::cast)
                    .find(|expr| !matches!(expr, ast::Expr::ArrayInit(_))),
                body,
            ) {
                // An expression body *is* the value, or is run for its effect when the interface returns none.
                (Some(value), _) => {
                    if matches!(returns, Ty::Void) {
                        lowering.discard(&value, &mut insn)?;
                    } else {
                        lowering.value_as(&value, &returns, &mut insn)?;
                    }
                    // An expression body leaves the value and writes no `return`, so the instruction is
                    // needed here — unlike a declared method, where a trailing one would be dead code.
                    insn.return_();
                }
                // A block body returns for itself; the trailing trap is the same dead code a declared body's
                // is, and is there so the validator need not infer Java's definite-return rule.
                (None, Some(block)) => {
                    lowering.block(&block, &mut insn)?;
                    if method.has_result {
                        insn.unreachable();
                    }
                }
                (None, None) => return Err(WasmError::Unsupported("a lambda with no body")),
            }
            return Ok(Self {
                locals: lowering.locals,
                code: insn.into_body(),
            });
        }
        if let Some(params) = method.node.children().find_map(ast::ParamList::cast) {
            for param in params.params() {
                let ty = lowering.declare_param(param.syntax())?;
                let _ = ty;
            }
        }
        // The captures are trailing *parameters*, so their slots follow the declared ones — reserved
        // before any body-local claims them.
        let first_capture = lowering.next;
        lowering.next += u32::try_from(method.captures).unwrap_or(0);

        let mut insn = Insn::new();
        let block = method.node.children().find_map(ast::Block::cast);
        // An instance field initialiser is not a statement anywhere in the source, so a constructor
        // that emitted only its own body left every one of them unrun — a field reading back as its
        // type's default in a module that validates. A `this(…)` delegation is the exception: the
        // constructor it reaches runs them, and running them twice would undo what it did.
        // The synthetic field is written before anything else, so an initialiser or the body can already
        // reach the enclosing instance through it.
        if let (Some(owner), Some(_)) = (method.owner, method.encloses)
            && let Some(&slot) = layout.outer.get(&owner)
        {
            insn.local_get(0)
                .local_get(1)
                .struct_set(layout.structs[&owner], slot);
        }
        // Each capture's parameter goes into its field, before anything else can read it.
        if let Some(owner) = method.owner
            && method.captures > 0
            && let Some(&first_field) = layout.capture_slot.get(&owner)
        {
            for offset in 0..u32::try_from(method.captures).unwrap_or(0) {
                insn.local_get(0)
                    .local_get(first_capture + offset)
                    .struct_set(layout.structs[&owner], first_field + offset);
            }
        }
        if method.is_constructor && !block.as_ref().is_some_and(Self::delegates_to_this) {
            // The implicit `super()`, which the source writes only when it has arguments to pass. It
            // runs the superclass's initialisers, and without it every inherited field read back as its
            // default in a module that validates.
            if let Some(owner) = method.owner
                && !block.as_ref().is_some_and(Self::delegates_to_super)
                && let Some(function) = Self::super_constructor(owner, index, layout)
            {
                insn.local_get(0).call(function);
            }
            // The constructor's parent *is* the class body, which is where the initialisers are and
            // the reason they need no search: they are this declaration's siblings, in order.
            if let (Some(owner), Some(body)) = (method.owner, method.node.parent()) {
                lowering.initializers(owner, &body, 0, &mut insn)?;
            }
        }
        if let Some(block) = &block {
            lowering.block(block, &mut insn)?;
        }
        // A body that returns on every Java path can still *fall out* of a wasm block: a `br` sitting in
        // unreachable code does not make its target reachable, so the validator sees control reach the
        // end of the function with nothing on the stack. Java's definite-return rule is what makes this
        // dead code; the instruction is here so the validator does not have to infer that.
        if method.has_result {
            insn.unreachable();
        }
        Ok(Self {
            locals: lowering.locals,
            code: insn.into_body(),
        })
    }
}

impl Body {
    /// Whether a constructor body begins with `this(…)` rather than `super(…)` or a statement.
    fn delegates_to_this(block: &ast::Block) -> bool {
        Facts::body_delegates_to(block, jals_syntax::SyntaxKind::THIS_KW)
    }

    /// Whether a constructor body begins with an explicit `super(…)`.
    fn delegates_to_super(block: &ast::Block) -> bool {
        Facts::body_delegates_to(block, jals_syntax::SyntaxKind::SUPER_KW)
    }

    /// The function an implicit `super()` calls: the nearest ancestor with initialisers to run.
    ///
    /// A subclass's construction runs its superclass's field initialisers first (JLS §12.5), and
    /// leaving that out read every inherited field back as its default in a module that validates. The
    /// walk continues past an ancestor that has no constructor function of its own, because *its*
    /// superclass may still have one — a class with no initialisers is a link in the chain, not its end.
    ///
    /// `None` at a class whose declared constructors all take arguments: Java requires an explicit
    /// `super(…)` there, so there is nothing implicit to call, and the source wrote what to run.
    fn super_constructor(owner: ItemId, index: &ProjectIndex, layout: &Layout) -> Option<u32> {
        let mut candidate = CompileWasm::superclass(owner, index);
        while let Some(item) = candidate {
            let mut declared = index
                .own_members(item)
                .iter()
                .copied()
                .filter(|&member| index.member(member).kind == DefKind::Constructor)
                .peekable();
            if declared.peek().is_some() {
                return declared
                    .find(|&member| index.member(member).params.is_empty())
                    .and_then(|member| layout.functions.get(&member).copied());
            }
            if let Some(&function) = layout.default_constructors.get(&item) {
                return Some(function);
            }
            candidate = CompileWasm::superclass(item, index);
        }
        None
    }
}

/// The mutable state of lowering one body.
struct Lowering<'a> {
    input: &'a TypedFile<'a>,
    index: &'a ProjectIndex,
    layout: &'a Layout,
    /// `(definition, local index)` pairs, parameters first.
    slots: Vec<(DefId, u32)>,
    /// Locals beyond the parameters, in declaration order.
    locals: Vec<ValType>,
    /// The next free local index. Unlike the JVM's, a wasm local is one slot whatever its width.
    next: u32,
    owner: Option<ItemId>,
    /// Enclosing `break` / `continue` targets, innermost last.
    loops: Vec<Loop>,
    /// A label read off a `LabeledStmt`, waiting for the loop it labels to claim it.
    pending_label: Option<String>,
    /// Enclosing `finally` blocks, innermost last: a `return` runs every one of them on its way out.
    cleanups: Vec<ast::Block>,
    /// Enclosing `switch` *expressions*, innermost last: where a `yield` branches to, and the type the
    /// value it carries must have.
    yields: Vec<(u32, ValType)>,
}

/// One arm of a lowered `switch`: which keys reach it, in the order the arms are written.
///
/// It carries no entry label, unlike the JVM backend's: an arm's entry is a *position* in the block
/// nesting rather than a name, and the position is the arm's index.
struct Arm {
    /// The `case` keys that reach this arm. Empty for a bare `default`.
    keys: Vec<i32>,
    /// The `case T t` patterns that reach this arm, in the order they are written.
    ///
    /// A pattern is not a constant, so it indexes no `br_table`: a `switch` with one dispatches by
    /// testing each arm's type in source order, which is what §14.11.1 says a pattern `switch` does.
    patterns: Vec<SyntaxNode>,
    /// The arm's `when` clause, which runs after the pattern bound and before the arm is taken.
    guard: Option<ast::Expr>,
    /// Whether one of this arm's labels is `default`.
    is_default: bool,
}

/// One enclosing statement a `break` or a `continue` can name.
///
/// Both depths are `Insn::depth()` values taken just after the structure opened, so a branch is the
/// *difference* against the depth at the branch — the only way to get it right when an `if` may have
/// opened in between.
struct Loop {
    /// The Java label on this statement, if it has one.
    label: Option<String>,
    /// Where a `break` lands: past the whole statement.
    leave: u32,
    /// Where a `continue` lands, or `None` for a labelled statement that is no loop.
    repeat: Option<u32>,
    /// How many `finally` blocks were open when this statement was entered. A jump out of it has to run
    /// every cleanup opened *since* — those are the ones it leaves behind.
    cleanups: usize,
}

impl Lowering<'_> {
    /// A lowering with no receiver, for the start function: it has no `this`, no parameters, and no
    /// enclosing loop, and it carries the locals a previous input's share of the body already used so
    /// two inputs never claim the same index.
    fn for_static<'a>(
        input: &'a TypedFile<'a>,
        index: &'a ProjectIndex,
        layout: &'a Layout,
        locals: Vec<ValType>,
    ) -> Lowering<'a> {
        let next = u32::try_from(locals.len()).unwrap_or(u32::MAX);
        Lowering {
            input,
            index,
            layout,
            slots: Vec::new(),
            locals,
            next,
            owner: None,
            loops: Vec::new(),
            pending_label: None,
            cleanups: Vec::new(),
            yields: Vec::new(),
        }
    }

    /// `<global> = <value>` in the start function, with the assignment conversion the constant
    /// expression could not hold.
    fn assign_static(
        &mut self,
        value: &ast::Expr,
        declared: &Ty,
        global: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        self.value_as(value, declared, insn)?;
        insn.global_set(global);
        Ok(())
    }

    /// Build one `enum` constant: allocate it, then run the constructor its arguments select.
    ///
    /// The allocation alone is not the finished object. A constant is the one `new` an `enum` has, and
    /// leaving out the constructor left every field at its default — a module that validates and reads
    /// back zero. Selection is by arity: a constant's argument list is not an expression the index
    /// resolved a call target for, so there is nothing to read a selection out of.
    fn enum_constant(&mut self, owner: ItemId, node: &SyntaxNode, insn: &mut Insn) -> Result<()> {
        let arguments: Vec<ast::Expr> = node
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        // A constant with a body is an instance of its *own* subclass, which is where its overrides
        // live. Its global still has the enum's type, so nothing else changes.
        let built = if node.children().any(|child| child.kind() == CLASS_BODY) {
            self.index
                .item_by_decl(self.input.file(), usize::from(node.text_range().start()))
                .ok_or(WasmError::Unsupported("an `enum` constant with no item"))?
        } else {
            owner
        };
        let mut matching = self
            .index
            .own_members(owner)
            .iter()
            .copied()
            .filter(|&member| {
                let info = self.index.member(member);
                info.kind == DefKind::Constructor && info.params.len() == arguments.len()
            });
        let selected = matching.next();
        if selected.is_some() && matching.next().is_some() {
            return Err(WasmError::Unsupported(
                "an `enum` with two constructors of one arity",
            ));
        }
        if selected.is_none() && !arguments.is_empty() {
            return Err(WasmError::Unsupported(
                "an `enum` constant with no matching constructor",
            ));
        }
        let ty = self.layout.class_ref(built)?;
        insn.struct_new_default(self.layout.structs[&built]);
        // The receiver is stored and re-read rather than duplicated, wasm having no `dup`, and the
        // global takes it from there.
        let slot = self.scratch(ty);
        insn.local_set(slot);
        // The enum's own construction: the constructor the arguments selected, or — when the enum
        // declares none — the synthesised one that runs its field initialisers.
        let enum_constructor = match selected {
            Some(constructor) => Some(
                *self
                    .layout
                    .functions
                    .get(&constructor)
                    .ok_or(WasmError::Unsupported("an `enum` constructor with no body"))?,
            ),
            None => self.layout.default_constructors.get(&owner).copied(),
        };
        if let Some(function) = enum_constructor {
            insn.local_get(slot);
            for argument in &arguments {
                self.expr(argument, insn)?;
            }
            insn.call(function);
        }
        // Then the body's *own* initialisers, which belong to a second class and run after the enum's
        // (§12.5). Nothing else reaches them: the enum's constructor knows nothing of the subclass, and
        // running them from the body's synthesised constructor's own `super()` would run the enum's
        // twice — which is why that call is skipped under an `enum`.
        if built != owner
            && let Some(&initialise) = self.layout.default_constructors.get(&built)
        {
            insn.local_get(slot).call(initialise);
        }
        insn.local_get(slot);
        Ok(())
    }

    /// Every instance initialiser the enclosing class declares, in source order.
    ///
    /// Two forms interleave: a field's `= …`, and a bare `{ … }` block. JLS §12.5 runs them in the
    /// order they are *written*, one sequence, before the constructor's own body — which is why they
    /// are emitted from the class body's children here rather than reached through `stmt`. A
    /// `FIELD_DECL` is not a statement, and a `{ … }` in a class body is not the same node as one in a
    /// method.
    fn initializers(
        &mut self,
        owner: ItemId,
        class_body: &SyntaxNode,
        receiver: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        let struct_type =
            *self.layout.structs.get(&owner).ok_or_else(|| {
                WasmError::NoRepresentation(self.index.item(owner).fqn.to_string())
            })?;
        for node in class_body.children() {
            if node.kind() == INITIALIZER {
                // The `static` keyword is inside the `MODIFIERS` child, not on the `INITIALIZER`
                // itself. A `static { … }` runs once at class initialisation rather than per instance,
                // and this backend has no start function to run it in — so it is reported rather than
                // run in every constructor, which would be a different program.
                // A `static { … }` runs once in the module's start function, not per instance.
                if node
                    .children()
                    .filter(|child| child.kind() == MODIFIERS)
                    .flat_map(|modifiers| modifiers.children_with_tokens())
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|token| token.kind() == jals_syntax::SyntaxKind::STATIC_KW)
                {
                    continue;
                }
                if let Some(block) = node.children().find_map(ast::Block::cast) {
                    self.block(&block, insn)?;
                }
                continue;
            }
            let Some(declaration) = ast::FieldDecl::cast(node.clone()) else {
                continue;
            };
            let names: Vec<SyntaxToken> = declaration.names().collect();
            let values: Vec<ast::Expr> = node.children().filter_map(ast::Expr::cast).collect();
            for (position, name) in names.iter().enumerate() {
                let Some(value) = values.get(position) else {
                    continue;
                };
                let Some(member) = self
                    .index
                    .member_by_decl(self.input.file(), usize::from(name.text_range().start()))
                else {
                    continue;
                };
                if self.index.member(member).modifiers.is_static {
                    continue;
                }
                let Some(slot) = self.layout.field_slot(owner, member) else {
                    continue;
                };
                insn.local_get(receiver);
                let declared = self.index.resolved_member_ty(member);
                self.value_as(value, &declared, insn)?;
                insn.struct_set(struct_type, slot);
            }
        }
        Ok(())
    }

    fn declare_param(&mut self, node: &SyntaxNode) -> Result<ValType> {
        let id = self
            .facts()
            .def_at(node)
            .ok_or(WasmError::Unsupported("an unresolved parameter"))?;
        let ty = self.layout.val_type(self.input.type_of_def(id))?;
        self.slots.push((id, self.next));
        self.next += 1;
        Ok(ty)
    }

    fn declare_local(&mut self, id: DefId) -> Result<u32> {
        let ty = self.layout.val_type(self.input.type_of_def(id))?;
        let slot = self.next;
        self.slots.push((id, slot));
        self.locals.push(ty);
        self.next += 1;
        Ok(slot)
    }

    /// The local `id` is bound to, searched from the most recent binding: a `catch` arm rebinds its
    /// variable once per declared type, and the copy being lowered must see its *own* local.
    fn slot_of(&self, id: DefId) -> Option<u32> {
        self.slots
            .iter()
            .rev()
            .find(|(entry, _)| *entry == id)
            .map(|(_, slot)| *slot)
    }

    /// The source facts of the file being lowered.
    ///
    /// A projection, not a store: [`Facts`] is a `Copy` handle over the same [`TypedFile`] this
    /// lowering already holds. It is where the span keying, the name binding, and the constant
    /// evaluation live, so neither backend spells them itself.
    const fn facts(&self) -> Facts<'_> {
        Facts::of(*self.input)
    }

    fn ty_of(&self, node: &SyntaxNode) -> Result<ValType> {
        let ty = self
            .input
            .type_of_expr(Facts::span(node))
            .ok_or(WasmError::Unsupported(
                "an expression with no inferred type",
            ))?;
        self.layout.val_type(ty)
    }

    // --- statements ---------------------------------------------------------

    fn block(&mut self, block: &ast::Block, insn: &mut Insn) -> Result<()> {
        for statement in block.stmts() {
            self.stmt(&statement, insn)?;
        }
        Ok(())
    }

    fn stmt(&mut self, statement: &ast::Stmt, insn: &mut Insn) -> Result<()> {
        match statement {
            ast::Stmt::Block(block) => self.block(block, insn),
            // `;` has nothing to emit, and neither has an `assert`: Java evaluates one only when
            // assertions are *enabled*, they are disabled by default, and a wasm host has no `-ea` to
            // turn them on. So an `assert` compiles to nothing, which is exactly what a JVM does with
            // one by default — the condition is still parsed, resolved, and linted, it simply has no
            // run-time effect. A trap would be *stricter* than Java.
            ast::Stmt::Empty(_) | ast::Stmt::Assert(_) => Ok(()),
            ast::Stmt::LocalVar(declaration) => self.local(declaration, insn),
            ast::Stmt::Expr(expression) => {
                let Some(value) = expression.expr() else {
                    return Ok(());
                };
                self.discard(&value, insn)
            }
            ast::Stmt::Return(statement) => {
                // The value is computed *first*, then every enclosing `finally` runs, then the frame
                // leaves — which is the order §14.20.2 gives and the reason a cleanup can observe the
                // value's side effects but not change what is returned.
                if let Some(value) = statement.expr() {
                    self.expr(&value, insn)?;
                }
                // A `return` leaves the frame, so it leaves *every* open cleanup behind.
                self.run_cleanups(0, insn)?;
                insn.return_();
                Ok(())
            }
            ast::Stmt::If(statement) => self.conditional(statement, insn),
            ast::Stmt::While(statement) => self.while_loop(statement, insn),
            ast::Stmt::DoWhile(statement) => self.do_while(statement, insn),
            ast::Stmt::For(statement) => self.for_loop(statement, insn),
            ast::Stmt::ForEach(statement) => self.for_each(statement, insn),
            ast::Stmt::Break(statement) => self.leave(statement.syntax(), false, insn),
            ast::Stmt::Continue(statement) => self.leave(statement.syntax(), true, insn),
            ast::Stmt::Labeled(statement) => self.labelled(statement, insn),
            // Each of these names itself rather than going through a catch-all, so a report says which
            // construct is missing. All four wait on the same thing: the exception-handling proposal's
            // `tag` section and `try_table`, which `encode.rs` does not write yet. `synchronized` waits
            // on it too — its body is `finally`-protected, and a monitor this host does not have is the
            // smaller half of the problem.
            ast::Stmt::Throw(statement) => self.throw(statement, insn),
            ast::Stmt::Try(statement) => self.try_catch(statement, insn),
            ast::Stmt::Synchronized(statement) => self.synchronized(statement, insn),
            ast::Stmt::Yield(statement) => {
                let value = statement
                    .expr()
                    .ok_or(WasmError::Unsupported("a `yield` with no value"))?;
                let (leave, ty) = *self.yields.last().ok_or(WasmError::Unsupported(
                    "a `yield` outside a `switch` expression",
                ))?;
                self.arm_value(&value, ty, insn)?;
                insn.br(insn.depth() - leave);
                Ok(())
            }
            ast::Stmt::Switch(statement) => {
                let selector = statement
                    .selector()
                    .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
                let body = statement
                    .body()
                    .ok_or(WasmError::Unsupported("a `switch` with no body"))?;
                self.switch(&selector, &body, None, insn)
            }
        }
    }

    fn local(&mut self, declaration: &ast::LocalVarDecl, insn: &mut Insn) -> Result<()> {
        let names: Vec<_> = declaration.names().collect();
        let values: Vec<_> = declaration
            .syntax()
            .children()
            .filter_map(ast::Expr::cast)
            .collect();
        for (position, name) in names.iter().enumerate() {
            let id = self
                .input
                .analysis()
                .symbol_at(usize::from(name.text_range().start()))
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let slot = self.declare_local(id)?;
            if let Some(value) = values.get(position) {
                let declared = self.input.type_of_def(id).clone();
                self.value_as(value, &declared, insn)?;
                insn.local_set(slot);
            }
        }
        Ok(())
    }

    /// `if` is wasm's own instruction, so the source's nesting is the output's.
    fn conditional(&mut self, statement: &ast::IfStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("an `if` with no condition"))?;
        let mut branches = statement.branches();
        let then_branch = branches.next();
        let else_branch = branches.next();

        self.expr(&condition, insn)?;
        insn.if_();
        if let Some(then) = then_branch {
            self.stmt(&then, insn)?;
        }
        if let Some(otherwise) = else_branch {
            insn.else_();
            self.stmt(&otherwise, insn)?;
        }
        insn.end();
        Ok(())
    }

    /// `while` is a `block` around a `loop`: leaving branches out of the block, repeating branches to
    /// the loop. The two labels are why wasm needs both instructions — a `loop` alone can only jump
    /// backwards, and a `block` alone only forwards.
    ///
    /// A `continue` re-tests the condition, so the loop *is* its target here; the other two loop forms
    /// need a third structure because their continuation point is not the top.
    fn while_loop(&mut self, statement: &ast::WhileStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("a `while` with no condition"))?;
        let label = self.pending_label.take();
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        self.expr(&condition, insn)?;
        insn.i32_eqz();
        insn.br_if(insn.depth() - leave);
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(repeat),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `do body while (cond)`.
    ///
    /// Three structures, not two: a `continue` reaches the *bottom* test, which is neither the top of
    /// the loop nor past the end of it. The inner block is that point.
    fn do_while(&mut self, statement: &ast::DoWhileStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("a `do` with no condition"))?;
        let label = self.pending_label.take();
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        self.expr(&condition, insn)?;
        insn.br_if(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `for (init; condition; update) body`.
    ///
    /// A `continue` runs the update before re-testing (JLS §14.14.1.3), so the update sits *between*
    /// the inner block's end and the branch back — which is exactly what makes the inner block the
    /// continue target rather than the loop.
    fn for_loop(&mut self, statement: &ast::ForStmt, insn: &mut Insn) -> Result<()> {
        let label = self.pending_label.take();
        for node in statement.init() {
            self.for_section(&node, insn)?;
        }
        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        // No condition means `for (;;)`, which never leaves by itself.
        if let Some(condition) = statement.condition() {
            self.expr(&condition, insn)?;
            insn.i32_eqz();
            insn.br_if(insn.depth() - leave);
        }
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        for node in statement.update() {
            self.for_section(&node, insn)?;
        }
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// One node of a `for` header's initialiser or update list: a declaration, or an expression run for
    /// its effect.
    fn for_section(&mut self, node: &SyntaxNode, insn: &mut Insn) -> Result<()> {
        if let Some(declaration) = ast::LocalVarDecl::cast(node.clone()) {
            return self.local(&declaration, insn);
        }
        let expression =
            ast::Expr::cast(node.clone()).ok_or(WasmError::Unsupported("this `for` header"))?;
        self.discard(&expression, insn)
    }

    /// `for (T v : array) body`, over an array.
    ///
    /// JLS §14.14.2 defines it as an indexed loop, and this is that loop: the array and the index live
    /// in scratch locals so neither the iterable expression nor `array.len` is re-evaluated per step.
    /// An `Iterable` would need `java.lang.Iterable`, which this host does not have.
    fn for_each(&mut self, statement: &ast::ForEachStmt, insn: &mut Insn) -> Result<()> {
        let iterable = statement
            .iterable()
            .ok_or(WasmError::Unsupported("a `for`-each over nothing"))?;
        let name: SyntaxToken = statement
            .name_token()
            .ok_or(WasmError::Unsupported("a `for`-each with no variable"))?;
        let Some(Ty::Array(element)) = self.input.type_of_expr(Facts::span(iterable.syntax()))
        else {
            return Err(WasmError::Unsupported("a `for`-each over this type"));
        };
        let element = self.layout.val_type(element)?;
        let array_type = self
            .layout
            .array_type(element)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        let label = self.pending_label.take();

        let array_ty = self
            .expr(&iterable, insn)?
            .ok_or(WasmError::Unsupported("a `for`-each over no value"))?;
        let array = self.scratch(array_ty);
        insn.local_set(array);
        let index = self.scratch(ValType::I32);
        insn.i32_const(0).local_set(index);

        let id = self
            .input
            .analysis()
            .symbol_at(usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
        let variable = self.declare_local(id)?;

        insn.block();
        let leave = insn.depth();
        insn.loop_();
        let repeat = insn.depth();
        // `index < array.length`, then leave when it is not.
        insn.local_get(index)
            .local_get(array)
            .array_len()
            .numeric(NumOp::Lt, ValType::I32)
            .ok_or(WasmError::Unsupported("an array length comparison"))?;
        insn.i32_eqz();
        insn.br_if(insn.depth() - leave);
        // The variable is bound before the continue target, so a `continue` cannot skip the binding.
        insn.local_get(array)
            .local_get(index)
            .array_get(array_type)
            .local_set(variable);
        insn.block();
        let next = insn.depth();
        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: Some(next),
            cleanups,
        });
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        self.loops.pop();
        insn.end();
        insn.local_get(index)
            .i32_const(1)
            .numeric(NumOp::Add, ValType::I32)
            .ok_or(WasmError::Unsupported("an index increment"))?;
        insn.local_set(index);
        insn.br(insn.depth() - repeat).end().end();
        Ok(())
    }

    /// `label: statement`.
    ///
    /// A labelled *loop* takes the label into its own entry, so `continue label` reaches its update
    /// rather than merely leaving it. Anything else gets a block of its own, which only `break label`
    /// can target — the label of a non-loop is a forward jump and nothing else (JLS §14.7).
    fn labelled(&mut self, statement: &ast::LabeledStmt, insn: &mut Insn) -> Result<()> {
        let label = statement
            .label()
            .ok_or(WasmError::Unsupported("a label with no name"))?;
        let inner = statement
            .stmt()
            .ok_or(WasmError::Unsupported("a label with no statement"))?;
        if matches!(
            inner,
            ast::Stmt::While(_) | ast::Stmt::DoWhile(_) | ast::Stmt::For(_) | ast::Stmt::ForEach(_)
        ) {
            self.pending_label = Some(label);
            return self.stmt(&inner, insn);
        }
        insn.block();
        let leave = insn.depth();
        self.loops.push(Loop {
            label: Some(label),
            leave,
            repeat: None,
            cleanups: self.cleanups.len(),
        });
        let lowered = self.stmt(&inner, insn);
        self.loops.pop();
        lowered?;
        insn.end();
        Ok(())
    }

    /// `synchronized (lock) { … }`.
    ///
    /// There is no monitor on this host: a wasm module here is single-threaded, so there is nothing for
    /// a lock to exclude and nothing for a `finally` to release. What remains of the statement is its
    /// two observable effects — the lock expression is evaluated, and a `null` one fails. It *traps*
    /// rather than throwing a `NullPointerException`, which is the same trade this backend already makes
    /// for a failed `ref.cast` on a host with no exception model to throw into.
    fn synchronized(&mut self, statement: &ast::SynchronizedStmt, insn: &mut Insn) -> Result<()> {
        let lock = statement
            .syntax()
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(WasmError::Unsupported("a `synchronized` with no lock"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `synchronized` with no body"))?;
        self.expr(&lock, insn)?
            .ok_or(WasmError::Unsupported("a `synchronized` on no value"))?;
        insn.ref_as_non_null().drop();
        self.block(&body, insn)
    }

    /// `throw e`.
    ///
    /// One tag carries every Java exception, because every one of them is a reference: what a `catch`
    /// tests is the *class* of the payload, not which tag raised it.
    fn throw(&mut self, statement: &ast::ThrowStmt, insn: &mut Insn) -> Result<()> {
        let value = statement
            .expr()
            .ok_or(WasmError::Unsupported("a `throw` with nothing to throw"))?;
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `throw` with no tag declared"))?;
        self.expr(&value, insn)?
            .ok_or(WasmError::Unsupported("a `throw` of no value"))?;
        insn.throw(tag);
        Ok(())
    }

    /// `try { … } catch (T v) { … }`, with as many handlers as the source wrote.
    ///
    /// `try_table` delivers the payload to *one* label, so the class tests happen after it rather than
    /// in it: the caught reference is spilled into a local and each handler is a `ref.test` against its
    /// declared type, in source order (§14.20 — the first matching clause wins). A payload no clause
    /// accepts is re-thrown, which is what makes an unhandled exception leave the frame rather than
    /// being swallowed.
    ///
    /// `finally` and try-with-resources are reported: both need their block duplicated onto every exit
    /// path, including the branch out of the `try` and the re-throw, and neither is emitted here yet.
    fn try_catch(&mut self, statement: &ast::TryStmt, insn: &mut Insn) -> Result<()> {
        use jals_syntax::SyntaxKind::{FINALLY_CLAUSE, RESOURCE_LIST};
        let finally = statement
            .syntax()
            .children()
            .find(|child| child.kind() == FINALLY_CLAUSE)
            .and_then(|clause| clause.children().find_map(ast::Block::cast));
        // A resource is declared, used, and closed: the declaration becomes a local here and the close
        // becomes part of the cleanup, so the rest of this function needs to know only that there is one.
        let resources: Vec<ast::Resource> = statement
            .syntax()
            .children()
            .filter(|child| child.kind() == RESOURCE_LIST)
            .flat_map(|list| list.children().filter_map(ast::Resource::cast))
            .collect();
        let clauses: Vec<ast::CatchClause> = statement
            .syntax()
            .children()
            .filter_map(ast::CatchClause::cast)
            .collect();
        // Resources alone are the whole statement. With a `catch` or a `finally` beside them, §14.20.3
        // makes the resource `try` the *body* of an ordinary one — so the outer structure below wraps it
        // rather than duplicating the close sequence into every handler.
        if !resources.is_empty() && clauses.is_empty() && finally.is_none() {
            return self.try_resources(statement, &resources, insn);
        }
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `try` with no tag declared"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `try` with no body"))?;
        if clauses.is_empty() && finally.is_none() {
            return Err(WasmError::Unsupported("a `try` with no handler"));
        }
        // A `finally` has to run on *every* way out, and a `return` / `break` / `continue` inside the
        // protected code is a way out this lowering does not intercept — it would branch straight past
        // the block the cleanup sits after. Reported rather than emitted with the cleanup skipped, which
        // would be a silent one.

        if let Some(cleanup) = &finally {
            self.cleanups.push(cleanup.clone());
        }
        insn.block();
        let out = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let handler = insn.depth();
        insn.try_table(&[(tag, insn.depth() - handler)]);
        if resources.is_empty() {
            self.block(&body, insn)?;
        } else {
            self.try_resources(statement, &resources, insn)?;
        }
        insn.end();
        // The body completed, so nothing was caught: leave past every handler.
        insn.br(insn.depth() - out);
        insn.end();

        // The caught reference, which each clause narrows in turn.
        let caught = self.scratch(ValType::Ref(RefType::nullable(HeapType::Any)));
        insn.local_set(caught);
        for clause in &clauses {
            self.catch_clause(clause, caught, out, insn)?;
        }
        // Popped before the cleanup's own copies are emitted: a `return` inside the `finally` must not
        // run the `finally` again.
        if finally.is_some() {
            self.cleanups.pop();
        }
        // Nothing matched: run the cleanup and re-throw, so the exception leaves this frame rather than
        // vanishing. This is the copy of `finally` the *exceptional* path needs; the normal path gets
        // its own below, which is the duplication a structured cleanup costs.
        if let Some(cleanup) = &finally {
            self.block(cleanup, insn)?;
        }
        insn.local_get(caught);
        insn.throw(tag);
        insn.end();
        if let Some(cleanup) = &finally {
            self.block(cleanup, insn)?;
        }
        Ok(())
    }

    /// `try (R r = …) { … }`.
    ///
    /// §14.20.3 closes each resource in reverse declaration order, on both the normal and the exceptional
    /// path, skipping a `null` one. What this does *not* do is record a suppressed exception: a `close()`
    /// that throws while the body is already throwing is swallowed, because `Throwable.addSuppressed`
    /// needs a type with no wasm representation. The *primary* exception is still the body's, which is the
    /// one Java propagates and the one a `catch` sees — so the control flow is right and only the
    /// suppressed list is missing.
    ///
    /// A `catch` or a `finally` beside the resources is not this function's business: §14.20.3 makes the
    /// resource `try` the *body* of an ordinary one, so [`try_catch`](Self::try_catch) wraps this.
    fn try_resources(
        &mut self,
        statement: &ast::TryStmt,
        resources: &[ast::Resource],
        insn: &mut Insn,
    ) -> Result<()> {
        let tag = self
            .layout
            .tag
            .ok_or(WasmError::Unsupported("a `try` with no tag declared"))?;
        let body = statement
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `try` with no body"))?;

        // Each resource is a local of its declared type, initialised in order.
        let mut slots = Vec::with_capacity(resources.len());
        for resource in resources {
            let name = resource
                .binding()
                .ok_or(WasmError::Unsupported("a resource with no name"))?;
            let value = resource
                .syntax()
                .children()
                .find_map(ast::Expr::cast)
                .ok_or(WasmError::Unsupported("a resource with no initialiser"))?;
            let id = self
                .input
                .analysis()
                .symbol_at(usize::from(name.text_range().start()))
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let slot = self.declare_local(id)?;
            let declared = self.input.type_of_def(id).clone();
            self.value_as(&value, &declared, insn)?;
            insn.local_set(slot);
            slots.push((slot, declared));
        }

        insn.block();
        let out = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let handler = insn.depth();
        insn.try_table(&[(tag, insn.depth() - handler)]);
        self.block(&body, insn)?;
        insn.end();
        // The body completed: close normally, so a `close()` that throws propagates.
        self.close_resources(&slots, insn)?;
        insn.br(insn.depth() - out);
        insn.end();

        let caught = self.scratch(ValType::Ref(RefType::nullable(HeapType::Any)));
        insn.local_set(caught);
        insn.block();
        let closed = insn.depth();
        insn.block_typed(ValType::Ref(RefType::nullable(HeapType::Any)));
        let threw = insn.depth();
        insn.try_table(&[(tag, insn.depth() - threw)]);
        self.close_resources(&slots, insn)?;
        insn.br(insn.depth() - closed);
        insn.end();
        // Only reachable if `close()` completed without branching, which it cannot.
        insn.unreachable();
        insn.end();
        // The suppressed exception, dropped rather than attached: see this function's own note.
        insn.drop();
        insn.end();
        insn.local_get(caught);
        insn.throw(tag);
        insn.end();
        Ok(())
    }

    /// Call a no-argument `void` method on a receiver already in a local, on its *runtime* type.
    ///
    /// The same `ref.test` chain a call site builds, without the call site: the overrides are a known,
    /// closed set with the whole project in one module, so testing them most-derived first answers what a
    /// vtable would. Used where there is no call expression to read a receiver out of — a resource's
    /// `close`, which §14.20.3 calls and the source never writes.
    fn dispatch_to(&self, receiver: u32, member: MemberId, insn: &mut Insn) -> Result<()> {
        let overriders = self.overriders(member);
        let fallback = self.layout.functions.get(&member).copied();
        if overriders.is_empty() {
            let function = fallback.ok_or(WasmError::Unsupported("a `close` with no body"))?;
            insn.local_get(receiver).call(function);
            return Ok(());
        }
        insn.block();
        let leave = insn.depth();
        for &(item, over) in &overriders {
            let Some(&function) = self.layout.functions.get(&over) else {
                continue;
            };
            let struct_type = self.layout.structs[&item];
            insn.local_get(receiver);
            insn.ref_test(HeapType::Concrete(struct_type), false);
            insn.if_();
            insn.local_get(receiver);
            insn.ref_cast(HeapType::Concrete(struct_type), false);
            insn.call(function);
            insn.br(insn.depth() - leave);
            insn.end();
        }
        // An interface's method is abstract: every class that could satisfy the call is already in the
        // chain, so reaching here means a receiver of a type nothing implemented, which traps.
        match fallback {
            Some(function) => {
                insn.local_get(receiver);
                insn.call(function);
            }
            None => {
                insn.unreachable();
            }
        }
        insn.end();
        Ok(())
    }

    /// `if (r != null) r.close();` for each resource, in reverse declaration order.
    fn close_resources(&self, slots: &[(u32, Ty)], insn: &mut Insn) -> Result<()> {
        for (slot, declared) in slots.iter().rev() {
            let item = declared
                .project_id()
                .ok_or(WasmError::Unsupported("a resource of an unindexed type"))?;
            let close = self
                .index
                .members_of(item)
                .into_iter()
                .find(|&id| {
                    let member = self.index.member(id);
                    member.kind == DefKind::Method
                        && member.name == "close"
                        && member.params.is_empty()
                })
                .ok_or(WasmError::Unsupported("a resource with no `close`"))?;
            // `r != null`, which §14.20.3 checks before closing.
            insn.local_get(*slot);
            insn.ref_is_null();
            insn.i32_eqz();
            insn.if_();
            // The runtime type decides which `close` runs, exactly as it does at a call site: the
            // declared type is what named the method, and a subclass may have overridden it.
            self.dispatch_to(*slot, close, insn)?;
            insn.end();
        }
        Ok(())
    }

    /// One `catch (T v) { … }`: test the payload's class, bind it, run the block, and leave.
    ///
    /// A multi-catch (`catch (A | B v)`) is several tests reaching one block, which is why the types
    /// are a list here rather than one.
    fn catch_clause(
        &mut self,
        clause: &ast::CatchClause,
        caught: u32,
        out: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        let types: Vec<ast::Type> = clause
            .syntax()
            .descendants()
            .filter_map(ast::Type::cast)
            .collect();
        if types.is_empty() {
            return Err(WasmError::Unsupported("a `catch` with no type"));
        }
        let name = clause
            .binding()
            .ok_or(WasmError::Unsupported("a `catch` with no variable"))?;
        let body = clause
            .syntax()
            .children()
            .find_map(ast::Block::cast)
            .ok_or(WasmError::Unsupported("a `catch` with no body"))?;

        // A multi-catch is lowered as one arm *per declared type* rather than one arm testing several.
        // The variable's type is the least upper bound of the declared types, so any member the source
        // can legally reach through it is declared on that bound — and a struct's fields start with its
        // supertype's, so the slot is the same in every one of them. Narrowing to the concrete type per
        // copy is therefore sound, and it is the only way to give the variable a wasm type at all: there
        // is no struct type for a bound this backend does not compute.
        let id = self
            .input
            .analysis()
            .symbol_at(usize::from(name.text_range().start()))
            .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
        for ty in &types {
            let heap = self.named_type(ty)?;
            insn.local_get(caught);
            insn.ref_test(heap, false);
            insn.if_();
            let declared = self.ty_of_type(ty)?;
            let slot = self.scratch(declared);
            insn.local_get(caught);
            insn.ref_cast(heap, true);
            insn.local_set(slot);
            self.slots.push((id, slot));
            let lowered = self.block(&body, insn);
            self.slots.pop();
            lowered?;
            insn.br(insn.depth() - out);
            insn.end();
        }
        Ok(())
    }

    /// The wasm type a written `TYPE` node names, for a binding the analysis records no type for.
    fn ty_of_type(&self, ty: &ast::Type) -> Result<ValType> {
        let HeapType::Concrete(index) = self.named_type(ty)? else {
            return Err(WasmError::Unsupported("a `catch` type with no struct type"));
        };
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(index))))
    }

    /// `switch (selector) { … }`, statement or expression.
    ///
    /// The shape is one `block` per arm, nested inside a `block` for the whole `switch`: a `br i` from
    /// the dispatch lands just past arm `i`'s block, which is where arm `i`'s body starts. Falling out
    /// of arm `i`'s block end then runs arm `i+1` — so the colon form's fallthrough is what the nesting
    /// *already does*, with no branch of its own.
    fn switch(
        &mut self,
        selector: &ast::Expr,
        body: &ast::SwitchBlock,
        result: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<()> {
        let rules: Vec<ast::SwitchRule> = body.rules().collect();
        let groups: Vec<ast::SwitchGroup> = body.groups().collect();
        if !rules.is_empty() && !groups.is_empty() {
            // JLS §14.11.1 forbids mixing them, so this is not a program.
            return Err(WasmError::Unsupported("a `switch` mixing both forms"));
        }
        let arms: Vec<Arm> = if rules.is_empty() {
            groups
                .iter()
                .map(|group| self.arm(group.labels()))
                .collect::<Result<_>>()?
        } else {
            rules
                .iter()
                .map(|rule| self.arm(rule.label().into_iter()))
                .collect::<Result<_>>()?
        };
        let count = u32::try_from(arms.len()).map_err(|_| WasmError::TooLarge)?;
        // An unmatched key has to reach *some* label, and for an expression that label cannot be the
        // end of the block: the block owes a value. Exhaustiveness over an `enum` is the other way to
        // satisfy §14.11.2 and is not lowered, so a `default` is required rather than assumed.
        let default = arms.iter().position(|arm| arm.is_default);
        if result.is_some() && default.is_none() {
            return Err(WasmError::Unsupported(
                "a `switch` expression with no `default`",
            ));
        }
        let fallback = default.map_or(count, |index| u32::try_from(index).unwrap_or(count));
        let label = self.pending_label.take();

        match result {
            Some(ty) => insn.block_typed(ty),
            None => insn.block(),
        };
        let leave = insn.depth();
        for _ in 0..count {
            insn.block();
        }
        self.dispatch(selector, &arms, fallback, insn)?;

        let cleanups = self.cleanups.len();
        self.loops.push(Loop {
            label,
            leave,
            repeat: None,
            cleanups,
        });
        if let Some(ty) = result {
            self.yields.push((leave, ty));
        }
        let lowered = if rules.is_empty() {
            self.switch_groups(&groups, result, insn)
        } else {
            self.switch_rules(&rules, result, leave, insn)
        };
        if result.is_some() {
            self.yields.pop();
        }
        self.loops.pop();
        lowered?;
        // A colon-form arm leaves by `yield`, so the last group's end carries no value. Java's own rule
        // is that every arm yields or throws; the instruction is here so the validator does not have to
        // infer that rule, exactly as a value-returning body's trailing one is.
        if result.is_some() {
            insn.unreachable();
        }
        insn.end();
        Ok(())
    }

    /// One arm's `case` keys and patterns. `default` contributes neither.
    fn arm(&self, labels: impl Iterator<Item = ast::SwitchLabel>) -> Result<Arm> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        let mut keys = Vec::new();
        let mut patterns = Vec::new();
        let mut guard = None;
        let mut is_default = false;
        for label in labels {
            if label.is_default() {
                is_default = true;
            }
            patterns.extend(label.syntax().children().filter(|child| {
                matches!(
                    child.kind(),
                    TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
                )
            }));
            if let Some(clause) = label.syntax().children().find_map(ast::Guard::cast) {
                guard = clause.condition();
                if guard.is_none() {
                    return Err(WasmError::Unsupported("a guarded `case`"));
                }
            }
            // A `Guard`'s condition is an expression child of the label too, so the keys are read only
            // when there is no guard to have contributed one.
            if guard.is_none() {
                for value in label.syntax().children().filter_map(ast::Expr::cast) {
                    // A `String` key has no wasm representation — this backend compiles primitives and
                    // project classes, and a host with no `java.base` has no `String` to hash.
                    keys.push(self.facts().case_key(&value)?.as_int().ok_or_else(|| {
                        WasmError::NoRepresentation("a `String` `case` label".to_owned())
                    })?);
                }
            }
        }
        Ok(Arm {
            keys,
            patterns,
            guard,
            is_default,
        })
    }

    /// A pattern `switch`: each arm's type is tested in source order, and the first match wins.
    ///
    /// No `br_table`, because a pattern is not a constant and there is nothing to index on. §14.11.1
    /// gives the first *matching* label, so the tests are emitted in the order they are written and a
    /// `default` is only reached by falling out of all of them — which is what `fallback` already is.
    /// The binding is stored inside the test that matched; a wasm local starts at its type's default,
    /// so the other arms need nothing.
    fn dispatch_patterns(
        &mut self,
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        // A constant beside a pattern would need the jump table this does not build.
        if arms.iter().any(|arm| !arm.keys.is_empty()) {
            return Err(WasmError::Unsupported("a `switch` mixing key types"));
        }
        let selector_ty = self
            .expr(selector, insn)?
            .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
        let scratch = self.scratch(selector_ty);
        insn.local_set(scratch);
        for (index, arm) in arms.iter().enumerate() {
            // A bare `default` matches nothing here: it is where the chain lands when every test failed.
            if arm.patterns.is_empty() && arm.guard.is_none() {
                continue;
            }
            let target = u32::try_from(index).map_err(|_| WasmError::TooLarge)?;
            insn.block();
            let next = insn.depth();
            for pattern in &arm.patterns {
                self.match_pattern(pattern, scratch, next, None, insn)?;
            }
            // The guard runs after the patterns bound, because it is written in terms of the bindings.
            if let Some(guard) = &arm.guard {
                self.expr(guard, insn)?
                    .ok_or(WasmError::Unsupported("a guarded `case`"))?;
                insn.i32_eqz();
                insn.br_if(insn.depth() - next);
            }
            // One block deeper than the depth the arms' blocks were opened at.
            insn.br(target + 1);
            insn.end();
        }
        insn.br(fallback);
        Ok(())
    }

    /// Emit the selector and the jump into the arms.
    fn dispatch(
        &mut self,
        selector: &ast::Expr,
        arms: &[Arm],
        fallback: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        if arms
            .iter()
            .any(|arm| !arm.patterns.is_empty() || arm.guard.is_some())
        {
            return self.dispatch_patterns(selector, arms, fallback, insn);
        }
        // The selector has to *already* be an `i32`. Converting one that is not would narrow it
        // silently: a `long` selector is not a Java program, but an `i32.wrap_i64` would turn it into
        // one that switches on the low 32 bits.
        if !matches!(
            self.input.type_of_expr(Facts::span(selector.syntax())),
            Some(Ty::Primitive(
                Primitive::Byte | Primitive::Short | Primitive::Char | Primitive::Int
            ))
        ) {
            return Err(WasmError::Unsupported("a `switch` on this selector type"));
        }
        let mut cases: Vec<(i32, u32)> = Vec::new();
        for (index, arm) in arms.iter().enumerate() {
            let target = u32::try_from(index).map_err(|_| WasmError::TooLarge)?;
            for &key in &arm.keys {
                cases.push((key, target));
            }
        }
        self.expr(selector, insn)?;
        let Some((&(first, _), rest)) = cases.split_first() else {
            // No `case` at all: the selector is still evaluated, and every key is the default.
            insn.drop().br(fallback);
            return Ok(());
        };
        let (min, max) = rest.iter().fold((first, first), |(low, high), &(key, _)| {
            (low.min(key), high.max(key))
        });
        // A table costs one entry per key in its range, present or not; the comparison chain costs
        // four instructions per key. Past a spread of a few empty slots per key the chain is smaller.
        let span = i64::from(max) - i64::from(min) + 1;
        if span <= 2 * i64::try_from(cases.len()).unwrap_or(i64::MAX) + 8 {
            let mut targets = alloc::vec![fallback; usize::try_from(span).unwrap_or(0)];
            for &(key, target) in &cases {
                let slot = usize::try_from(i64::from(key) - i64::from(min)).unwrap_or(0);
                // The first label wins, which is what a duplicate `case` would mean if it were legal.
                if targets[slot] == fallback {
                    targets[slot] = target;
                }
            }
            // `br_table` reads its index as *unsigned*, so subtracting the lowest key is the whole
            // bounds check: a key below it wraps past 2³¹ and lands on the default with the rest.
            if min != 0 {
                insn.i32_const(min);
                insn.numeric(NumOp::Sub, ValType::I32)
                    .ok_or(WasmError::Unsupported("a `switch` offset"))?;
            }
            insn.br_table(&targets, fallback);
            return Ok(());
        }
        // Sparse: wasm has no `lookupswitch`, so the keys are compared one at a time. The selector
        // lives in a local because each comparison consumes a copy of it.
        let slot = self.scratch(ValType::I32);
        insn.local_set(slot);
        for &(key, target) in &cases {
            insn.local_get(slot).i32_const(key);
            insn.numeric(NumOp::Eq, ValType::I32)
                .ok_or(WasmError::Unsupported("a `switch` comparison"))?;
            insn.br_if(target);
        }
        insn.br(fallback);
        Ok(())
    }

    /// The colon form's arms, which fall through into one another.
    fn switch_groups(
        &mut self,
        groups: &[ast::SwitchGroup],
        result: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<()> {
        // A value switch must leave every arm. The colon form falls through, so it is the *last*
        // group that decides whether the fall-out path exists — and that path has no value to
        // leave on the stack. Emitting it anyway produced a `block` with a declared result type
        // whose fall-out was filled with `unreachable`: a module that loads, validates, and traps.
        if result.is_some() && !groups.last().is_some_and(Facts::arm_leaves) {
            return Err(WasmError::Unsupported(
                "a `switch` expression arm that yields nothing",
            ));
        }
        for group in groups {
            insn.end();
            for statement in group.stmts() {
                self.stmt(&statement, insn)?;
            }
            // No branch: falling into the next group is what the colon form means, and a group that
            // wanted to stop said `break`.
        }
        Ok(())
    }

    /// The arrow form, where each arm stands alone and leaves the `switch` when it finishes.
    fn switch_rules(
        &mut self,
        rules: &[ast::SwitchRule],
        result: Option<ValType>,
        leave: u32,
        insn: &mut Insn,
    ) -> Result<()> {
        for rule in rules {
            insn.end();
            // Three body forms: an expression, a block, or a `throw`. In an expression `switch` the
            // first *is* the arm's value; in a statement one it is evaluated for its effect.
            if let Some(value) = rule.expr() {
                match result {
                    Some(ty) => self.arm_value(&value, ty, insn)?,
                    None => self.discard(&value, insn)?,
                }
                insn.br(insn.depth() - leave);
                continue;
            }
            if let Some(block) = rule.syntax().children().find_map(ast::Block::cast) {
                self.block(&block, insn)?;
            } else if let Some(thrown) = rule.syntax().children().find_map(ast::ThrowStmt::cast) {
                self.stmt(&ast::Stmt::Throw(thrown), insn)?;
            } else {
                return Err(WasmError::Unsupported("a `switch` arm of this form"));
            }
            // A block arm of an expression `switch` leaves by `yield`, which has already branched to the
            // same label carrying the value — so falling off the arm's own end is what Java's "every arm
            // yields or throws" rule says cannot happen, and branching here would branch with no value.
            // The instruction states that rule to the validator, exactly as the colon form's does.
            if result.is_some() {
                insn.unreachable();
            } else {
                insn.br(insn.depth() - leave);
            }
        }
        Ok(())
    }

    /// One arrow arm's value, converted to the type the whole `switch` expression has.
    fn arm_value(&mut self, value: &ast::Expr, ty: ValType, insn: &mut Insn) -> Result<()> {
        if self.num_of(value.syntax()).is_ok()
            && let Ok(target) = Self::num_for(ty)
        {
            return self.operand(value, target, insn);
        }
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("a `switch` arm with no value"))?;
        Ok(())
    }

    /// `break` / `break label` / `continue` / `continue label`.
    ///
    /// The branch depth comes from the emitter, not from the source: an `if` between a loop header and
    /// the branch shifts every target, and only the emitter knows how many structures are open.
    fn leave(&mut self, node: &SyntaxNode, continuing: bool, insn: &mut Insn) -> Result<()> {
        let label = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            .map(|token| token.text().to_owned());
        let target = self
            .loops
            .iter()
            .rev()
            .find(|entry| {
                let named = label
                    .as_ref()
                    .is_none_or(|wanted| entry.label.as_deref() == Some(wanted.as_str()));
                named && (!continuing || entry.repeat.is_some())
            })
            .ok_or(WasmError::Unsupported(
                "a `break` or `continue` with no enclosing target",
            ))?;
        let depth = if continuing {
            target.repeat.ok_or(WasmError::Unsupported(
                "a `continue` naming something that is no loop",
            ))?
        } else {
            target.leave
        };
        // Every `finally` opened *since* the target statement was entered is one this jump leaves
        // behind, so each runs on the way out, innermost first. A cleanup opened outside the target is
        // not left behind and must not run.
        let outer = target.cleanups;
        self.run_cleanups(outer, insn)?;
        insn.br(insn.depth() - depth);
        Ok(())
    }

    /// Emit the cleanups above `outer`, innermost first — the `finally` blocks a jump leaves behind.
    ///
    /// Each is lowered against the cleanups that enclose *it*, not against the whole open set. A
    /// `finally` is not protected by itself: a `return` inside one runs only what is outside it, and
    /// §14.20.2 gives that jump the abrupt completion — the enclosing `try` is already leaving.
    /// Lowering against the unshrunk set instead re-entered the same cleanup for every jump it
    /// contained, which recursed until the compiler's own stack ran out.
    ///
    /// The stack is restored afterwards because this is one *copy* of the cleanup, not the end of
    /// its scope: the exceptional and normal paths still have theirs to emit, and
    /// [`try_catch`](Self::try_catch) is what pops for good.
    fn run_cleanups(&mut self, outer: usize, insn: &mut Insn) -> Result<()> {
        let open = core::mem::take(&mut self.cleanups);
        let mut outcome = Ok(());
        for index in (outer.min(open.len())..open.len()).rev() {
            self.cleanups.clear();
            self.cleanups.extend_from_slice(&open[..index]);
            outcome = self.block(&open[index], insn);
            if outcome.is_err() {
                break;
            }
        }
        self.cleanups = open;
        outcome
    }

    // --- expressions --------------------------------------------------------

    /// Emit `expr`. Returns its type, or `None` when it left nothing on the stack.
    fn expr(&mut self, expr: &ast::Expr, insn: &mut Insn) -> Result<Option<ValType>> {
        match expr {
            ast::Expr::Literal(literal) => self.literal(literal, insn).map(Some),
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(WasmError::Unsupported("an empty parenthesis"))?;
                self.expr(&inner, insn)
            }
            // `this` parses as a name reference but carries no identifier token, so nothing
            // resolves it as a name; it is always local 0.
            ast::Expr::NameRef(_) if Facts::is_this(expr.syntax()) => {
                let owner = self
                    .owner
                    .ok_or(WasmError::Unsupported("`this` in a `static` method"))?;
                insn.local_get(0);
                self.layout.class_ref(owner).map(Some)
            }
            ast::Expr::NameRef(name) => self.name(name, insn).map(Some),
            ast::Expr::Binary(binary) => self.binary(binary, insn).map(Some),
            ast::Expr::Assignment(assignment) => self.assignment(assignment, true, insn),
            ast::Expr::New(new) => self.new_object(new, insn).map(Some),
            ast::Expr::Call(call) => self.call(call, insn),
            ast::Expr::Index(index) => self.index(index, insn).map(Some),
            ast::Expr::FieldAccess(access) => self.field(access, insn).map(Some),
            ast::Expr::Unary(unary) => self.unary(unary, true, insn),
            ast::Expr::Postfix(postfix) => {
                let (target, delta) = Self::postfix(postfix)?;
                self.update(&target, delta, false, true, insn)
            }
            ast::Expr::Ternary(ternary) => self.ternary(ternary, insn).map(Some),
            ast::Expr::Switch(switch) => {
                let selector = switch
                    .selector()
                    .ok_or(WasmError::Unsupported("a `switch` with no selector"))?;
                let body = switch
                    .body()
                    .ok_or(WasmError::Unsupported("a `switch` with no body"))?;
                let ty = self.ty_of(switch.syntax())?;
                self.switch(&selector, &body, Some(ty), insn)?;
                Ok(Some(ty))
            }
            ast::Expr::Cast(cast) => self.cast(cast, insn).map(Some),
            // Both need a function reference and a target type to make one *of*; the interface that
            // would be that type is not laid out yet either. Each names itself rather than sharing a
            // catch-all, which said only "this expression form".
            // A lambda *is* an instance of a one-method class here, so building one is building that: allocate
            // the struct and write the captures into it, exactly as an anonymous class's `new` does. There is
            // no `invokedynamic` to reach for and no need of one — the dispatch chain already finds the type.
            ast::Expr::MethodRef(reference) => {
                // The same object a lambda builds: the type is what the dispatch chain tests, and a delegating
                // reference captures nothing to write into it.
                let item = self
                    .index
                    .item_by_decl(self.input.file(), Facts::span(reference.syntax()).start)
                    .ok_or(WasmError::Unsupported("a method reference with no item"))?;
                let struct_type = *self
                    .layout
                    .structs
                    .get(&item)
                    .ok_or(WasmError::Unsupported(
                        "a method reference with no struct type",
                    ))?;
                let ty = self.layout.class_ref(item)?;
                insn.struct_new_default(struct_type);
                let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
                if !captured.is_empty() {
                    let slot = self.scratch(ty);
                    let first = *self
                        .layout
                        .capture_slot
                        .get(&item)
                        .ok_or(WasmError::Unsupported("a capture with no field"))?;
                    insn.local_set(slot);
                    for (offset, (id, _)) in captured.iter().enumerate() {
                        insn.local_get(slot);
                        self.push_capture(*id, insn)?;
                        let field =
                            first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                        insn.struct_set(struct_type, field);
                    }
                    insn.local_get(slot);
                }
                Ok(Some(ty))
            }
            ast::Expr::Lambda(lambda) => {
                let item = self
                    .index
                    .item_by_decl(self.input.file(), Facts::span(lambda.syntax()).start)
                    .ok_or(WasmError::Unsupported("a lambda with no item"))?;
                let struct_type = *self
                    .layout
                    .structs
                    .get(&item)
                    .ok_or(WasmError::Unsupported("a lambda with no struct type"))?;
                let ty = self.layout.class_ref(item)?;
                insn.struct_new_default(struct_type);
                let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
                if !captured.is_empty() {
                    let slot = self.scratch(ty);
                    let first = *self
                        .layout
                        .capture_slot
                        .get(&item)
                        .ok_or(WasmError::Unsupported("a capture with no field"))?;
                    insn.local_set(slot);
                    for (offset, (id, _)) in captured.iter().enumerate() {
                        insn.local_get(slot);
                        self.push_capture(*id, insn)?;
                        let field =
                            first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                        insn.struct_set(struct_type, field);
                    }
                    insn.local_get(slot);
                }
                Ok(Some(ty))
            }
            ast::Expr::ClassLiteral(_) => Err(WasmError::Unsupported("a `.class` literal")),
            // No target here, so the element type is whatever inference read off the elements. That is
            // right when they agree with the declaration and wrong when they do not — which is why a
            // declaration hands its own type down through `value_as` instead of coming through here.
            ast::Expr::ArrayInit(init) => self.array_initializer(init, None, insn).map(Some),
        }
    }

    /// Emit `expr` for its effect, dropping any value it leaves.
    ///
    /// An assignment and an increment are asked *not* to produce one rather than having it dropped
    /// afterwards: with no `dup`, producing it means a second load of a field or an array element, and
    /// an expression statement never wanted it.
    fn discard(&mut self, expr: &ast::Expr, insn: &mut Insn) -> Result<()> {
        match expr {
            ast::Expr::Assignment(assignment) => {
                self.assignment(assignment, false, insn)?;
            }
            ast::Expr::Unary(unary) => {
                if self.unary(unary, false, insn)?.is_some() {
                    insn.drop();
                }
            }
            ast::Expr::Postfix(postfix) => {
                let (target, delta) = Self::postfix(postfix)?;
                self.update(&target, delta, false, false, insn)?;
            }
            other => {
                if self.expr(other, insn)?.is_some() {
                    insn.drop();
                }
            }
        }
        Ok(())
    }

    /// The target and step of a postfix `++` / `--`.
    fn postfix(postfix: &ast::PostfixExpr) -> Result<(ast::Expr, i8)> {
        use jals_syntax::SyntaxKind::{MINUS_MINUS, PLUS_PLUS};
        let target = postfix
            .operand()
            .ok_or(WasmError::Unsupported("an increment of nothing"))?;
        let delta = postfix
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find_map(|token| match token.kind() {
                PLUS_PLUS => Some(1),
                MINUS_MINUS => Some(-1),
                _ => None,
            })
            .ok_or(WasmError::Unsupported("this postfix operator"))?;
        Ok((target, delta))
    }

    /// The member a field access names, and the class that declares it.
    ///
    /// A `this.`-qualified access is resolved against the enclosing class directly: `this` has no
    /// inferred type, so the analysis records no target for it.
    fn field_target(&self, access: &ast::FieldAccess) -> Result<(ItemId, MemberId)> {
        let name = access
            .field()
            .ok_or(WasmError::Unsupported("a field access with no name"))?;
        let unresolved = || WasmError::Unresolved(name.clone());
        let member = match access.receiver() {
            Some(receiver) if Facts::is_this(receiver.syntax()) => {
                let owner = self.owner.ok_or_else(unresolved)?;
                self.index
                    .resolve_member(owner, &name, jals_hir::Namespace::Value)
                    .ok_or_else(unresolved)?
            }
            _ => self
                .input
                .field_target_of(Facts::span(access.syntax()))
                .ok_or_else(unresolved)?,
        };
        Ok((self.index.member(member).owner, member))
    }

    fn literal(&self, literal: &ast::Literal, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{
            CHAR_LITERAL, FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, TRUE_KW,
        };
        let token = literal
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| !token.kind().is_trivia())
            .ok_or(WasmError::Unsupported("an empty literal"))?;
        // `null` has no type of its own, so it is answered before `ty_of` is asked for one.
        if token.kind() == NULL_KW {
            insn.ref_null(HeapType::None);
            return Ok(ValType::Ref(RefType::nullable(HeapType::None)));
        }
        let ty = self.ty_of(literal.syntax())?;
        let text = token.text();
        match token.kind() {
            TRUE_KW => {
                insn.i32_const(1);
            }
            FALSE_KW => {
                insn.i32_const(0);
            }
            INT_LITERAL => {
                // The shared fact, not the other backend's: `0xFF`, `0b1010`, `017`, and `1_000` all
                // mean what they mean in both, and reading them twice was two chances to disagree
                // about one of them. The width comes from the inferred type below, so the one the
                // fact reads off the suffix is dropped.
                let (value, _) = Literal::integer(text)?;
                match ty {
                    ValType::I64 => insn.i64_const(value),
                    _ => insn
                        .i32_const(i32::try_from(value).map_err(|_| {
                            WasmError::Unsupported("an out-of-range `int` literal")
                        })?),
                };
            }
            FLOAT_LITERAL => {
                let (value, _) = Literal::floating(text)?;
                #[expect(
                    clippy::cast_possible_truncation,
                    reason = "the inferred type says `f32`, and that narrowing is what a `float` \
                              constant is"
                )]
                match ty {
                    ValType::F32 => insn.f32_const(value as f32),
                    _ => insn.f64_const(value),
                };
            }
            // A `char` is an unsigned 16-bit integer, so it is an `i32` here like every other integral
            // type narrower than `long`. The escape reading is shared with the JVM backend: `'\n'` and
            // `'\u0041'` mean what they mean in both, and reading them twice would be two chances to
            // disagree about one of them.
            CHAR_LITERAL => {
                let value = Literal::character(text)?;
                match ty {
                    ValType::I64 => insn.i64_const(i64::from(u32::from(value))),
                    _ => insn.i32_const(i32::try_from(u32::from(value)).unwrap_or(0)),
                };
            }
            _ => return Err(WasmError::Unsupported("this literal kind")),
        }
        Ok(ty)
    }

    fn name(&self, name: &ast::NameRef, insn: &mut Insn) -> Result<ValType> {
        let text = name.syntax().text().to_string();
        let unresolved = || WasmError::Unresolved(text.trim().into());
        let member = match self.facts().def_at(name.syntax()) {
            Some(id) => {
                if let Some(slot) = self.slot_of(id) {
                    insn.local_get(slot);
                    return self.layout.val_type(self.input.type_of_def(id));
                }
                // A captured local is not a local *here*: it lives in the field the constructor filled.
                if let Some((field, ty)) = self.capture_field(id) {
                    let owner = self.owner.ok_or_else(unresolved)?;
                    insn.local_get(0)
                        .struct_get(self.layout.structs[&owner], field);
                    return self.layout.val_type(&ty);
                }
                // Not a local: a field of the enclosing class. A `static` one is a global and needs no
                // receiver; an instance one is reached through `this`, which is local 0.
                let declaration = self.input.analysis().def(id);
                self.index
                    .member_by_decl(self.input.file(), declaration.name_range.start)
            }
            // Nothing in the file declared it, which an *inherited* field never is.
            None => self.inherited_field(name.syntax()),
        };
        let member = member.ok_or_else(unresolved)?;
        if self.index.member(member).modifiers.is_static {
            let ty = self
                .layout
                .val_type(&self.index.resolved_member_ty(member))?;
            let global = self.layout.statics.get(&member).ok_or_else(unresolved)?;
            self.ensure_initialised(member, insn);
            insn.global_get(*global);
            return Ok(ty);
        }
        let owner = self.owner.ok_or_else(unresolved)?;
        let slot = self
            .layout
            .field_slot(owner, member)
            .ok_or_else(unresolved)?;
        let struct_type = self.layout.structs[&owner];
        insn.local_get(0).struct_get(struct_type, slot);
        self.layout.val_type(&self.index.resolved_member_ty(member))
    }

    /// Initialise the class that declares `member` before its `static` field is touched.
    ///
    /// JLS §12.4.1 initialises a class on its first *use*, and a `static` field access is one. The
    /// function guards itself, so all but the first call is a load and a branch — and a class reaching
    /// its own statics mid-initialisation gets the values written so far, which is what §12.4.2 says.
    fn ensure_initialised(&self, member: MemberId, insn: &mut Insn) {
        if let Some(&(function, _)) = self
            .layout
            .class_inits
            .get(&self.index.member(member).owner)
        {
            insn.call(function);
        }
    }

    /// The field an unqualified name reaches when nothing in the file declared it: one of a supertype's.
    ///
    /// Name resolution is file-local, and a superclass's field is not something it can see — it may not
    /// even be in this file. So the name is looked up on the enclosing type and then up the superclass
    /// chain, nearest first, which is the order that makes a shadowing field win. A struct holds its
    /// supertype's fields first, so the slot the inherited member lands in is the enclosing type's own.
    fn inherited_field(&self, node: &SyntaxNode) -> Option<MemberId> {
        let name = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)?;
        let mut candidate = self.owner;
        while let Some(item) = candidate {
            if let Some(member) = self
                .index
                .own_members(item)
                .iter()
                .copied()
                .find(|&member| {
                    let info = self.index.member(member);
                    info.kind == DefKind::Field && info.name == name.text()
                })
            {
                return Some(member);
            }
            candidate = CompileWasm::superclass(item, self.index);
        }
        None
    }

    /// `{1, 2, 3}`, whose elements are written rather than defaulted.
    ///
    /// An array initialiser has no type of its own — `{1, 2}` is an array of whatever it is assigned to
    /// — so the element type comes from the type inference recorded for the *declaration*. One
    /// instruction takes the values from the stack, so there is no allocate-then-fill sequence and no
    /// index to keep.
    fn array_initializer(
        &mut self,
        init: &ast::ArrayInit,
        target: Option<&Ty>,
        insn: &mut Insn,
    ) -> Result<ValType> {
        // The *target* decides the element type, not the elements: `long[] c = {1, 2}` is an `i64`
        // array whose elements happen to be written as `int` literals, and reading the type off the
        // elements built an `i32` array instead — a module the validator rejects, and the wrong type if
        // it had not.
        let inferred = self.input.type_of_expr(Facts::span(init.syntax())).cloned();
        let Some(Ty::Array(element)) = target.cloned().or(inferred) else {
            return Err(WasmError::Unsupported(
                "an array initialiser with no target type",
            ));
        };
        let element_ty = self.layout.val_type(&element)?;
        let array_type = self
            .layout
            .array_type(element_ty)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        let elements: Vec<ast::Expr> = init.elements().collect();
        let count = u32::try_from(elements.len()).map_err(|_| WasmError::TooLarge)?;
        for value in &elements {
            // A nested initialiser (`{{1}, {2}}`) reaches this same arm through `expr`, whose recorded
            // type is the inner array's — so nothing here has to know how deep it is.
            self.value_as(value, &element, insn)?;
        }
        insn.array_new_fixed(array_type, count);
        Ok(ValType::Ref(RefType::nullable(HeapType::Concrete(
            array_type,
        ))))
    }

    /// Emit `value` as a value of the declared type `declared`, converting where a numeric assignment
    /// conversion applies and handing a nested array initialiser its own element type.
    fn value_as(&mut self, value: &ast::Expr, declared: &Ty, insn: &mut Insn) -> Result<()> {
        if let ast::Expr::ArrayInit(nested) = value {
            self.array_initializer(nested, Some(declared), insn)?;
            return Ok(());
        }
        let declared_ty = self.layout.val_type(declared)?;
        if self.num_of(value.syntax()).is_ok()
            && let Ok(target) = Self::num_for(declared_ty)
        {
            return self.operand(value, target, insn);
        }
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("a value that produced nothing"))?;
        Ok(())
    }

    /// `array[index]`.
    fn index(&mut self, expr: &ast::IndexExpr, insn: &mut Insn) -> Result<ValType> {
        let mut parts = expr.parts();
        let array = parts
            .next()
            .ok_or(WasmError::Unsupported("an index with no array"))?;
        let subscript = parts
            .next()
            .ok_or(WasmError::Unsupported("an index with no subscript"))?;
        let element = self.ty_of(expr.syntax())?;
        let array_type = self
            .layout
            .array_type(element)
            .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
        self.expr(&array, insn)?;
        self.expr(&subscript, insn)?;
        insn.array_get(array_type);
        Ok(element)
    }

    fn field(&mut self, access: &ast::FieldAccess, insn: &mut Insn) -> Result<ValType> {
        // `array.length` is not a field at all — wasm gives an array its own instruction.
        if access.field().as_deref() == Some("length")
            && let Some(receiver) = access.receiver()
            && matches!(
                self.input.type_of_expr(Facts::span(receiver.syntax())),
                Some(Ty::Array(_))
            )
        {
            self.expr(&receiver, insn)?;
            insn.array_len();
            return Ok(ValType::I32);
        }
        let (owner, member) = self.field_target(access)?;
        // A `static` field is a global, so the receiver names only the *class*: it is not evaluated,
        // exactly as `getstatic` ignores one on the JVM.
        if self.index.member(member).modifiers.is_static {
            // No global means the field's type has none either, so asking for its `ValType` is what
            // produces the report — the same one a local of that type would get.
            let ty = self
                .layout
                .val_type(&self.index.resolved_member_ty(member))?;
            let global = self
                .layout
                .statics
                .get(&member)
                .ok_or_else(|| WasmError::Unresolved(access.field().unwrap_or_default()))?;
            self.ensure_initialised(member, insn);
            insn.global_get(*global);
            return Ok(ty);
        }
        let receiver = access
            .receiver()
            .ok_or(WasmError::Unsupported("a field access with no receiver"))?;
        self.expr(&receiver, insn)?;
        let slot = self
            .layout
            .field_slot(owner, member)
            .ok_or_else(|| WasmError::Unresolved(access.field().unwrap_or_default()))?;
        insn.struct_get(self.layout.structs[&owner], slot);
        self.layout.val_type(&self.index.resolved_member_ty(member))
    }

    fn unary(
        &mut self,
        unary: &ast::UnaryExpr,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        use jals_syntax::SyntaxKind::{BANG, MINUS, MINUS_MINUS, PLUS, PLUS_PLUS, TILDE};
        let operand = unary
            .operand()
            .ok_or(WasmError::Unsupported("a unary with no operand"))?;
        let operator = Facts::operator(unary.syntax());
        match operator.as_slice() {
            // A prefix `++` / `--` is an assignment, not an operator on a value.
            [PLUS_PLUS] => return self.update(&operand, 1, true, keep, insn),
            [MINUS_MINUS] => return self.update(&operand, -1, true, keep, insn),
            _ => {}
        }
        let ty = match operator.as_slice() {
            // `!b` flips a `boolean`, which is an `i32` that is 0 or 1 — so `i32.eqz` *is* the flip.
            [BANG] => {
                self.expr(&operand, insn)?
                    .ok_or(WasmError::Unsupported("a `!` on nothing"))?;
                insn.i32_eqz();
                ValType::I32
            }
            // `+` is not a no-op: unary numeric promotion still applies.
            [PLUS] => {
                let promoted = Self::promote_one(self.num_of(operand.syntax())?);
                self.operand(&operand, promoted, insn)?;
                promoted.val()
            }
            [MINUS] => {
                let promoted = Self::promote_one(self.num_of(operand.syntax())?);
                // wasm has no integer negation at all, so an integral `-x` is `0 - x` — which means
                // the zero goes on the stack *before* the operand.
                match promoted {
                    Num::Long => {
                        insn.i64_const(0);
                    }
                    Num::Float | Num::Double => {}
                    _ => {
                        insn.i32_const(0);
                    }
                }
                self.operand(&operand, promoted, insn)?;
                if matches!(promoted, Num::Float | Num::Double) {
                    insn.neg(promoted.val())
                        .ok_or(WasmError::Unsupported("this negation"))?;
                } else {
                    insn.numeric(NumOp::Sub, promoted.val())
                        .ok_or(WasmError::Unsupported("this negation"))?;
                }
                promoted.val()
            }
            // `~n` is `n ^ -1`, at the promoted width.
            [TILDE] => {
                let promoted = Self::promote_one(self.num_of(operand.syntax())?);
                self.operand(&operand, promoted, insn)?;
                match promoted {
                    Num::Long => insn.i64_const(-1),
                    _ => insn.i32_const(-1),
                };
                insn.numeric(NumOp::Xor, promoted.val())
                    .ok_or(WasmError::Unsupported("this complement"))?;
                promoted.val()
            }
            _ => return Err(WasmError::Unsupported("this unary operator")),
        };
        Ok(Some(ty))
    }

    /// `(T) e`.
    ///
    /// A primitive cast is a conversion; a reference cast is `ref.cast`, which *traps* rather than
    /// throwing a `ClassCastException` — the closest a host with no exception model gets, and the
    /// difference is worth knowing rather than hiding.
    fn cast(&mut self, cast: &ast::CastExpr, insn: &mut Insn) -> Result<ValType> {
        let operand = cast
            .expr()
            .ok_or(WasmError::Unsupported("a cast with no operand"))?;
        let ty = cast
            .ty()
            .ok_or(WasmError::Unsupported("a cast with no type"))?;
        if ty.is_primitive_or_var() {
            let target = self.num_of(cast.syntax())?;
            self.operand(&operand, target, insn)?;
            return Ok(target.val());
        }
        let heap = self.named_type(&ty)?;
        self.expr(&operand, insn)?
            .ok_or(WasmError::Unsupported("a cast of nothing"))?;
        insn.ref_cast(heap, true);
        Ok(ValType::Ref(RefType::nullable(heap)))
    }

    fn binary(&mut self, binary: &ast::BinaryExpr, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{
            AMP, AMP_AMP, BANG_EQ, CARET, EQ, EQ_EQ, GT, INSTANCEOF_KW, LSHIFT, LT, LT_EQ, MINUS,
            PERCENT, PIPE, PIPE_PIPE, PLUS, SLASH, STAR,
        };
        let operator = Facts::operator(binary.syntax());

        // Before the operands: an `instanceof` whose right side is a *pattern* has no right operand at
        // all — the pattern is a binding, not an expression, and asking for one reported the wrong thing.
        if operator.first() == Some(&INSTANCEOF_KW) {
            return self.instance_of(binary, insn);
        }
        let left = binary
            .lhs()
            .ok_or(WasmError::Unsupported("a binary with no left operand"))?;
        let right = binary
            .rhs()
            .ok_or(WasmError::Unsupported("a binary with no right operand"))?;

        // `&&` and `||` are not operators over two values: the right operand may not run at all.
        match operator.as_slice() {
            [AMP_AMP] => return self.short_circuit(&left, &right, true, insn),
            [PIPE_PIPE] => return self.short_circuit(&left, &right, false, insn),
            _ => {}
        }
        let op = match operator.as_slice() {
            [PLUS] => NumOp::Add,
            [MINUS] => NumOp::Sub,
            [STAR] => NumOp::Mul,
            [SLASH] => NumOp::Div,
            [PERCENT] => NumOp::Rem,
            [EQ_EQ] => NumOp::Eq,
            [BANG_EQ] => NumOp::Ne,
            [LT] => NumOp::Lt,
            [LT_EQ] => NumOp::Le,
            [GT] => NumOp::Gt,
            // `>=` is two tokens, so that `List<List<T>>` still closes as two `>`.
            [GT, EQ] => NumOp::Ge,
            [AMP] => NumOp::And,
            [PIPE] => NumOp::Or,
            [CARET] => NumOp::Xor,
            [LSHIFT] => NumOp::Shl,
            // `>>` and `>>>` are separate `>` tokens, for the same reason `>=` is.
            [GT, GT] => NumOp::Shr,
            [GT, GT, GT] => NumOp::Ushr,
            _ => return Err(WasmError::Unsupported("this binary operator")),
        };

        // A reference `==` / `!=` is identity, not arithmetic, and wasm spells it `ref.eq`.
        if matches!(op, NumOp::Eq | NumOp::Ne) && self.is_reference(left.syntax()) {
            return self.reference_equality(&left, &right, op == NumOp::Ne, insn);
        }

        let left_num = self.num_of(left.syntax())?;
        if op.is_shift() {
            // A shift promotes each side on its own, and wasm wants the *count* at the left operand's
            // own width: `i64.shl` takes two `i64`s where `lshl` takes a `long` and an `int`. So the
            // count is converted to the result's type rather than to `int`.
            let promoted = Self::promote_one(left_num);
            self.operand(&left, promoted, insn)?;
            self.operand(&right, promoted, insn)?;
            insn.numeric(op, promoted.val())
                .ok_or(WasmError::Unsupported("this operator on this type"))?;
            return Ok(promoted.val());
        }

        // Both operands share one type, because one opcode names one: `i64.add` over an `i32` is a
        // module the validator rejects. Java's binary numeric promotion says which.
        let promoted = Self::promote(left_num, self.num_of(right.syntax())?);
        self.operand(&left, promoted, insn)?;
        self.operand(&right, promoted, insn)?;
        insn.numeric(op, promoted.val())
            .ok_or(WasmError::Unsupported("this operator on this type"))?;
        // A comparison is a `boolean`, which is an `i32`; arithmetic keeps its operand type.
        Ok(match op {
            NumOp::Eq | NumOp::Ne | NumOp::Lt | NumOp::Le | NumOp::Gt | NumOp::Ge => ValType::I32,
            _ => promoted.val(),
        })
    }

    /// Emit `expr` and convert its value to `target`.
    fn operand(&mut self, expr: &ast::Expr, target: Num, insn: &mut Insn) -> Result<()> {
        let source = self.num_of(expr.syntax())?;
        self.expr(expr, insn)?
            .ok_or(WasmError::Unsupported("an operand that produced no value"))?;
        if source != target {
            insn.convert(source, target)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        Ok(())
    }

    /// The numeric type `node`'s recorded type is.
    fn num_of(&self, node: &SyntaxNode) -> Result<Num> {
        let ty = self
            .input
            .type_of_expr(Facts::span(node))
            .ok_or(WasmError::Unsupported("a value with no inferred type"))?;
        let Ty::Primitive(primitive) = ty else {
            return Err(WasmError::Unsupported("an arithmetic operand of this type"));
        };
        Ok(match primitive {
            Primitive::Byte => Num::Byte,
            Primitive::Short => Num::Short,
            Primitive::Char => Num::Char,
            // A `boolean` shares `int`'s representation, and the only operators it reaches are the
            // bitwise ones, where that is exactly right.
            Primitive::Int | Primitive::Boolean => Num::Int,
            Primitive::Long => Num::Long,
            Primitive::Float => Num::Float,
            Primitive::Double => Num::Double,
        })
    }

    /// Whether `node`'s recorded type is a reference.
    fn is_reference(&self, node: &SyntaxNode) -> bool {
        matches!(
            self.input.type_of_expr(Facts::span(node)),
            Some(Ty::Class(_) | Ty::Array(_) | Ty::Null)
        )
    }

    /// Binary numeric promotion (JLS §5.6.2).
    const fn promote(left: Num, right: Num) -> Num {
        match (left, right) {
            (Num::Double, _) | (_, Num::Double) => Num::Double,
            (Num::Float, _) | (_, Num::Float) => Num::Float,
            (Num::Long, _) | (_, Num::Long) => Num::Long,
            _ => Num::Int,
        }
    }

    /// Unary numeric promotion (JLS §5.6.1).
    const fn promote_one(num: Num) -> Num {
        match num {
            Num::Byte | Num::Short | Num::Char | Num::Int => Num::Int,
            other => other,
        }
    }

    /// `a == b` / `a != b` over two references, which is identity.
    fn reference_equality(
        &mut self,
        left: &ast::Expr,
        right: &ast::Expr,
        negated: bool,
        insn: &mut Insn,
    ) -> Result<ValType> {
        // `x == null` has no second reference to compare: `ref.null` would need the *other* side's
        // type, and `ref.is_null` asks the question directly.
        let (value, other) = if Self::is_null_literal(right.syntax()) {
            (left, None)
        } else if Self::is_null_literal(left.syntax()) {
            (right, None)
        } else {
            (left, Some(right))
        };
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("a comparison operand with no value"))?;
        match other {
            Some(other) => {
                self.expr(other, insn)?
                    .ok_or(WasmError::Unsupported("a comparison operand with no value"))?;
                insn.ref_eq();
            }
            None => {
                insn.ref_is_null();
            }
        }
        if negated {
            insn.i32_eqz();
        }
        Ok(ValType::I32)
    }

    /// Whether `node` is the `null` literal.
    fn is_null_literal(node: &SyntaxNode) -> bool {
        node.descendants_with_tokens()
            .any(|element| element.kind() == jals_syntax::SyntaxKind::NULL_KW)
    }

    /// `e instanceof T`.
    ///
    /// `ref.test` answers it, with one difference the lowering has to close: its nullable form is
    /// *true* for a `null`, and Java's `instanceof` is false for one. So the non-nullable form is used,
    /// which is exactly the question Java asks.
    fn instance_of(&mut self, binary: &ast::BinaryExpr, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        let operand = binary
            .lhs()
            .ok_or(WasmError::Unsupported("an `instanceof` with no operand"))?;
        let pattern = binary.syntax().children().find(|child| {
            matches!(
                child.kind(),
                TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
            )
        });
        // A plain type test binds nothing, so it is the test and nothing else.
        let Some(pattern) = pattern else {
            let ty = binary
                .syntax()
                .children()
                .find_map(ast::Type::cast)
                .ok_or(WasmError::Unsupported("an `instanceof` with no type"))?;
            let target = self.named_type(&ty)?;
            self.expr(&operand, insn)?
                .ok_or(WasmError::Unsupported("an `instanceof` on nothing"))?;
            insn.ref_test(target, false);
            return Ok(ValType::I32);
        };
        let operand_ty = self
            .expr(&operand, insn)?
            .ok_or(WasmError::Unsupported("an `instanceof` on nothing"))?;
        let scratch = self.scratch(operand_ty);
        let answer = self.scratch(ValType::I32);
        insn.local_set(scratch);
        // A wasm local starts at its type's default, so a binding the match did not reach needs nothing
        // arranged for it — unlike the JVM's, where the verifier merges both paths at the join.
        insn.i32_const(0).local_set(answer);
        insn.block();
        let fail = insn.depth();
        self.match_pattern(&pattern, scratch, fail, None, insn)?;
        insn.i32_const(1).local_set(answer);
        insn.end();
        insn.local_get(answer);
        Ok(ValType::I32)
    }

    /// Match `pattern` against the value in `value`, branching out to `fail` when it does not.
    ///
    /// Falls through on a match, with every binding written. A *record* pattern is the recursive case:
    /// it tests the type, then reads each component through its *accessor* — which is what a
    /// deconstruction calls (§14.30.1), a record being free to declare one by hand — and matches the
    /// component pattern against that.
    fn match_pattern(
        &mut self,
        pattern: &SyntaxNode,
        value: u32,
        fail: u32,
        declared: Option<&Ty>,
        insn: &mut Insn,
    ) -> Result<()> {
        use jals_syntax::SyntaxKind::{RECORD_PATTERN, TYPE_PATTERN, UNNAMED_PATTERN};
        match pattern.kind() {
            // `_` matches anything and binds nothing, so there is nothing to emit.
            UNNAMED_PATTERN => Ok(()),
            TYPE_PATTERN => {
                let bound = self
                    .facts()
                    .def_at(pattern)
                    .ok_or(WasmError::Unsupported("a pattern with no binding"))?;
                let bound_ty = self.input.type_of_def(bound).clone();
                let slot = self.declare_local(bound)?;
                // Two cases carry no test. A primitive one because a `ref` instruction over it is not a
                // program. And a component pattern of the component's *own* type because it matches
                // unconditionally (§14.30.2) — including a `null` component, which a `ref.test` would
                // reject and so drop a match Java makes.
                if matches!(bound_ty, Ty::Primitive(_)) || declared == Some(&bound_ty) {
                    insn.local_get(value).local_set(slot);
                    return Ok(());
                }
                let ty = pattern
                    .children()
                    .find_map(ast::Type::cast)
                    .ok_or(WasmError::Unsupported("a pattern with no type"))?;
                let target = self.named_type(&ty)?;
                insn.local_get(value).ref_test(target, false).i32_eqz();
                insn.br_if(insn.depth() - fail);
                insn.local_get(value).ref_cast(target, false);
                insn.local_set(slot);
                Ok(())
            }
            RECORD_PATTERN => {
                let ty = pattern
                    .children()
                    .find_map(ast::Type::cast)
                    .ok_or(WasmError::Unsupported("a `record` pattern with no type"))?;
                let target = self.named_type(&ty)?;
                let item = self
                    .index
                    .resolve_type_name(
                        self.input.file(),
                        &ty.simple_name()
                            .ok_or(WasmError::Unsupported("a type with no name"))?,
                        None,
                    )
                    .project_id()
                    .ok_or(WasmError::Unsupported("a `record` pattern on no record"))?;
                insn.local_get(value).ref_test(target, false).i32_eqz();
                insn.br_if(insn.depth() - fail);
                let narrowed = self.scratch(self.layout.class_ref(item)?);
                insn.local_get(value).ref_cast(target, false);
                insn.local_set(narrowed);
                // The components in header order, which is the order the sub-patterns are written in.
                let components: Vec<MemberId> = self
                    .index
                    .own_members(item)
                    .iter()
                    .copied()
                    .filter(|&member| {
                        let info = self.index.member(member);
                        info.kind == DefKind::Field && !info.modifiers.is_static
                    })
                    .collect();
                let subs: Vec<SyntaxNode> = pattern
                    .children()
                    .filter(|child| {
                        matches!(
                            child.kind(),
                            TYPE_PATTERN | RECORD_PATTERN | UNNAMED_PATTERN
                        )
                    })
                    .collect();
                if subs.len() != components.len() {
                    return Err(WasmError::Unsupported(
                        "a `record` pattern of the wrong arity",
                    ));
                }
                for (component, sub) in components.iter().zip(&subs) {
                    let name = self.index.member(*component).name.clone();
                    let accessor = self
                        .index
                        .own_members(item)
                        .iter()
                        .copied()
                        .find(|&member| {
                            let info = self.index.member(member);
                            info.kind == DefKind::Method
                                && info.name == name
                                && info.params.is_empty()
                        })
                        .ok_or(WasmError::Unsupported(
                            "a record component with no accessor",
                        ))?;
                    let function = *self
                        .layout
                        .functions
                        .get(&accessor)
                        .ok_or(WasmError::Unsupported("a record accessor with no body"))?;
                    let held = self.scratch(
                        self.layout
                            .val_type(&self.index.resolved_member_ty(*component))?,
                    );
                    insn.local_get(narrowed).call(function);
                    insn.local_set(held);
                    let component_ty = self.index.resolved_member_ty(*component);
                    self.match_pattern(sub, held, fail, Some(&component_ty), insn)?;
                }
                Ok(())
            }
            _ => Err(WasmError::Unsupported("an `instanceof` pattern")),
        }
    }

    /// The declared heap type a `TYPE` node names.
    fn named_type(&self, ty: &ast::Type) -> Result<HeapType> {
        let name = ty
            .simple_name()
            .ok_or(WasmError::Unsupported("a type with no name"))?;
        let qualified = ty.is_qualified().then(|| ty.qualified_text()).flatten();
        let item = self
            .index
            .resolve_type_name(self.input.file(), &name, qualified.as_deref())
            .project_id()
            .ok_or_else(|| WasmError::Unresolved(name.clone()))?;
        let index = self
            .layout
            .structs
            .get(&item)
            .ok_or(WasmError::NoRepresentation(name))?;
        Ok(HeapType::Concrete(*index))
    }

    /// An assignment, simple or compound. Returns the assigned type when `keep` asked for the value.
    fn assignment(
        &mut self,
        assignment: &ast::AssignmentExpr,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let target = assignment
            .target()
            .ok_or(WasmError::Unsupported("an assignment with no target"))?;
        let value = assignment
            .value()
            .ok_or(WasmError::Unsupported("an assignment with no value"))?;
        let place = self.place(&target, insn)?;

        if assignment.is_simple() {
            place.address(insn);
            let source = self.num_of(value.syntax()).ok();
            self.expr(&value, insn)?
                .ok_or(WasmError::Unsupported("an assignment of no value"))?;
            // Assignment conversion (JLS §5.2): `long n = 1` stores a `long`, and the literal is an
            // `int` until something widens it. Only a numeric target needs it; a reference one is
            // already the right type or the analysis would not have typed the assignment.
            if let (Some(source), Ok(declared)) = (source, self.num_of(target.syntax()))
                && source != declared
            {
                insn.convert(source, declared)
                    .ok_or(WasmError::Unsupported("this assignment conversion"))?;
            }
            place.store(insn, keep);
        } else {
            let operation = Self::compound_operator(assignment.syntax())?;
            self.compound(&place, &target, &value, operation, keep, insn)?;
        }

        if keep { Ok(Some(place.ty())) } else { Ok(None) }
    }

    /// The operator a compound assignment applies. `=` is not one of them.
    fn compound_operator(node: &SyntaxNode) -> Result<NumOp> {
        use jals_syntax::SyntaxKind::{
            AMP_EQ, CARET_EQ, EQ, GT, LSHIFT_EQ, MINUS_EQ, PERCENT_EQ, PIPE_EQ, PLUS_EQ, SLASH_EQ,
            STAR_EQ,
        };
        let operator: Vec<_> = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect();
        Ok(match operator.as_slice() {
            [PLUS_EQ] => NumOp::Add,
            [MINUS_EQ] => NumOp::Sub,
            [STAR_EQ] => NumOp::Mul,
            [SLASH_EQ] => NumOp::Div,
            [PERCENT_EQ] => NumOp::Rem,
            [AMP_EQ] => NumOp::And,
            [PIPE_EQ] => NumOp::Or,
            [CARET_EQ] => NumOp::Xor,
            [LSHIFT_EQ] => NumOp::Shl,
            // `>>=` and `>>>=` are separate `>` tokens, so that `List<List<T>>` still closes.
            [GT, GT, EQ] => NumOp::Shr,
            [GT, GT, GT, EQ] => NumOp::Ushr,
            _ => return Err(WasmError::Unsupported("this compound assignment operator")),
        })
    }

    /// `E1 op= E2`, which JLS §15.26.2 defines as `E1 = (T)((E1) op (E2))` for `E1`'s type `T`.
    ///
    /// Both conversions carry weight: the operator runs at the *promoted* type, so `int i; i += 1L`
    /// widens `i` to `i64` and wraps the sum back, and `byte b; b += 1` adds as `i32` and sign-extends
    /// the low byte. Dropping either stores a value outside the variable's range in a module that
    /// validates.
    fn compound(
        &mut self,
        place: &Place,
        target: &ast::Expr,
        value: &ast::Expr,
        operation: NumOp,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<()> {
        let declared = self.num_of(target.syntax())?;
        place.address(insn);
        place.read(insn);
        let promoted = if operation.is_shift() {
            Self::promote_one(declared)
        } else {
            Self::promote(declared, self.num_of(value.syntax())?)
        };
        if declared != promoted {
            insn.convert(declared, promoted)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        self.operand(value, promoted, insn)?;
        insn.numeric(operation, promoted.val())
            .ok_or(WasmError::Unsupported("this operator on this type"))?;
        if promoted != declared {
            insn.convert(promoted, declared)
                .ok_or(WasmError::Unsupported("this narrowing"))?;
        }
        place.store(insn, keep);
        Ok(())
    }

    /// `++e` / `--e` / `e++` / `e--`.
    ///
    /// The postfix and prefix forms differ only in *when* the place is read for the result, and with no
    /// `dup` that difference is a read before the write rather than after it.
    fn update(
        &mut self,
        target: &ast::Expr,
        delta: i8,
        prefix: bool,
        keep: bool,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        let declared = self.num_of(target.syntax())?;
        let place = self.place(target, insn)?;
        // The postfix form yields the value the variable *had*, so it is spilled before the store.
        let previous = if keep && !prefix {
            let slot = self.scratch(place.ty());
            place.read(insn);
            insn.local_set(slot);
            Some(slot)
        } else {
            None
        };

        // `++` is `+= 1` with the same promotion and narrowing (§15.14.2), so a `char c; c++` adds as
        // `i32` and truncates back into a `char`.
        let promoted = Self::promote_one(declared);
        place.address(insn);
        place.read(insn);
        if declared != promoted {
            insn.convert(declared, promoted)
                .ok_or(WasmError::Unsupported("this conversion"))?;
        }
        Self::one(delta, promoted, insn);
        insn.numeric(NumOp::Add, promoted.val())
            .ok_or(WasmError::Unsupported("an increment of this type"))?;
        if promoted != declared {
            insn.convert(promoted, declared)
                .ok_or(WasmError::Unsupported("this narrowing"))?;
        }
        // The prefix form's result is the *new* value, which `store` can keep on the way past.
        place.store(insn, keep && prefix);

        match (keep, previous) {
            (false, _) => Ok(None),
            (true, Some(slot)) => {
                insn.local_get(slot);
                Ok(Some(place.ty()))
            }
            (true, None) => Ok(Some(place.ty())),
        }
    }

    /// The `1` (or `-1`) an increment adds, at the promoted type.
    fn one(delta: i8, promoted: Num, insn: &mut Insn) {
        match promoted {
            Num::Long => insn.i64_const(i64::from(delta)),
            Num::Float => insn.f32_const(f32::from(delta)),
            Num::Double => insn.f64_const(f64::from(delta)),
            Num::Byte | Num::Short | Num::Char | Num::Int => insn.i32_const(i32::from(delta)),
        };
    }

    /// Where an assignment's target lives, with its subexpressions already evaluated.
    ///
    /// This is the point of the type: a compound assignment reads its target *and* writes it, and
    /// §15.26.2 evaluates the target's subexpressions exactly once. With no `dup` to duplicate an
    /// address, the receiver of a field access and the array and index of a subscript are spilled into
    /// scratch locals here, and each access reads them back. The scratch locals are fresh per site, so
    /// `a[i++] += b[j++]` nests two of these without either clobbering the other.
    fn place(&mut self, target: &ast::Expr, insn: &mut Insn) -> Result<Place> {
        let ty = self.ty_of(target.syntax())?;
        match target {
            ast::Expr::Paren(paren) => {
                let inner = paren
                    .expr()
                    .ok_or(WasmError::Unsupported("an empty parenthesis"))?;
                self.place(&inner, insn)
            }
            ast::Expr::Index(subscript) => {
                let mut parts = subscript.parts();
                let array = parts
                    .next()
                    .ok_or(WasmError::Unsupported("an index with no array"))?;
                let index = parts
                    .next()
                    .ok_or(WasmError::Unsupported("an index with no subscript"))?;
                let array_type = self
                    .layout
                    .array_type(ty)
                    .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
                let array_value = self.expr(&array, insn)?.ok_or(WasmError::Unsupported(
                    "an index into something with no value",
                ))?;
                let array_slot = self.scratch(array_value);
                insn.local_set(array_slot);
                self.operand(&index, Num::Int, insn)?;
                let index_slot = self.scratch(ValType::I32);
                insn.local_set(index_slot);
                Ok(Place::Element {
                    array: array_slot,
                    index: index_slot,
                    array_type,
                    ty,
                })
            }
            ast::Expr::FieldAccess(access) => {
                let (owner, member) = self.field_target(access)?;
                if self.index.member(member).modifiers.is_static {
                    let global =
                        *self.layout.statics.get(&member).ok_or_else(|| {
                            WasmError::Unresolved(access.field().unwrap_or_default())
                        })?;
                    self.ensure_initialised(member, insn);
                    return Ok(Place::Global { index: global, ty });
                }
                let slot = self
                    .layout
                    .field_slot(owner, member)
                    .ok_or_else(|| WasmError::Unresolved(access.field().unwrap_or_default()))?;
                let receiver = access.receiver().ok_or(WasmError::Unsupported(
                    "a field assignment with no receiver",
                ))?;
                let receiver_ty = self
                    .expr(&receiver, insn)?
                    .ok_or(WasmError::Unsupported("a field of something with no value"))?;
                let receiver_slot = self.scratch(receiver_ty);
                insn.local_set(receiver_slot);
                Ok(Place::Field {
                    receiver: receiver_slot,
                    struct_type: self.layout.structs[&owner],
                    slot,
                    ty,
                })
            }
            ast::Expr::NameRef(name) => {
                let text = name.syntax().text().to_string();
                let unresolved = || WasmError::Unresolved(text.trim().into());
                let member = match self.facts().def_at(name.syntax()) {
                    Some(id) => {
                        if let Some(slot) = self.slot_of(id) {
                            return Ok(Place::Local { slot, ty });
                        }
                        // A bare name that is no local is a field of the enclosing class. A `static`
                        // one is a global; an instance one needs no spill, local 0 being a stable
                        // receiver already.
                        let declaration = self.input.analysis().def(id);
                        self.index
                            .member_by_decl(self.input.file(), declaration.name_range.start)
                    }
                    // Nothing in the file declared it, which an *inherited* field never is.
                    None => self.inherited_field(name.syntax()),
                };
                let member = member.ok_or_else(unresolved)?;
                if self.index.member(member).modifiers.is_static {
                    let global = *self.layout.statics.get(&member).ok_or_else(unresolved)?;
                    self.ensure_initialised(member, insn);
                    return Ok(Place::Global { index: global, ty });
                }
                let owner = self.owner.ok_or_else(unresolved)?;
                let slot = self
                    .layout
                    .field_slot(owner, member)
                    .ok_or_else(unresolved)?;
                Ok(Place::Field {
                    receiver: 0,
                    struct_type: self.layout.structs[&owner],
                    slot,
                    ty,
                })
            }
            _ => Err(WasmError::Unsupported("assignment to this target")),
        }
    }

    /// `c ? a : b`.
    ///
    /// A typed `if` rather than `select`, because `select` pops both value operands: both arms would
    /// already have run, and a trapping one would trap whether or not it was taken. §15.25 evaluates
    /// exactly one arm.
    fn ternary(&mut self, expr: &ast::TernaryExpr, insn: &mut Insn) -> Result<ValType> {
        let mut parts = expr.parts();
        let condition = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no condition"))?;
        let then_arm = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no then arm"))?;
        let else_arm = parts
            .next()
            .ok_or(WasmError::Unsupported("a `?:` with no else arm"))?;
        let ty = self.ty_of(expr.syntax())?;

        self.expr(&condition, insn)?;
        insn.if_typed(ty);
        self.ternary_arm(&then_arm, ty, insn)?;
        insn.else_();
        self.ternary_arm(&else_arm, ty, insn)?;
        insn.end();
        Ok(ty)
    }

    /// One arm of a `?:`, converted to the type the whole conditional has.
    ///
    /// The conversion is what makes `flag ? 1 : 2L` one `i64` block rather than a module the validator
    /// rejects for arms of different types.
    fn ternary_arm(&mut self, arm: &ast::Expr, ty: ValType, insn: &mut Insn) -> Result<()> {
        if self.num_of(arm.syntax()).is_ok()
            && let Ok(target) = Self::num_for(ty)
        {
            return self.operand(arm, target, insn);
        }
        self.expr(arm, insn)?
            .ok_or(WasmError::Unsupported("a `?:` arm with no value"))?;
        Ok(())
    }

    /// The numeric type a `ValType` is, for converting into a type an expression already has.
    const fn num_for(ty: ValType) -> Result<Num> {
        Ok(match ty {
            ValType::I32 => Num::Int,
            ValType::I64 => Num::Long,
            ValType::F32 => Num::Float,
            ValType::F64 => Num::Double,
            ValType::Ref(_) => return Err(WasmError::Unsupported("a numeric reference")),
        })
    }

    /// `a && b` / `a || b`, which evaluate `b` only when `a` did not already decide the answer.
    ///
    /// A typed `if` again, and for the same reason: the whole point of the operators is that the right
    /// operand may not run.
    fn short_circuit(
        &mut self,
        left: &ast::Expr,
        right: &ast::Expr,
        and: bool,
        insn: &mut Insn,
    ) -> Result<ValType> {
        self.expr(left, insn)?;
        insn.if_typed(ValType::I32);
        if and {
            self.expr(right, insn)?;
            insn.else_().i32_const(0);
        } else {
            insn.i32_const(1).else_();
            self.expr(right, insn)?;
        }
        insn.end();
        Ok(ValType::I32)
    }

    /// `new C(args)`: allocate with every field at its default, then run the constructor on it.
    ///
    /// The allocation is `struct.new_default`, and from that instruction on the object belongs to
    /// the host's collector. There is no header, no allocation site bookkeeping, and nothing to
    /// free.
    fn new_object(&mut self, new: &ast::NewExpr, insn: &mut Insn) -> Result<ValType> {
        let ty = self.ty_of(new.syntax())?;
        // `new T[n]`: one instruction, and every element starts at its type's default — which is
        // exactly Java's rule for a fresh array.
        if let Some(Ty::Array(element)) = self.input.type_of_expr(Facts::span(new.syntax())) {
            let element = self.layout.val_type(element)?;
            let array_type = self
                .layout
                .array_type(element)
                .ok_or_else(|| WasmError::NoRepresentation("an array".to_owned()))?;
            let length = new
                .syntax()
                .children()
                .find_map(ast::Expr::cast)
                .ok_or(WasmError::Unsupported("an array creation with no length"))?;
            self.expr(&length, insn)?;
            insn.array_new_default(array_type);
            return Ok(ty);
        }
        // An anonymous class is its own type, and the `new` builds *that* rather than the type it named.
        let anonymous = CompileWasm::is_anonymous(new.syntax());
        let item = if anonymous {
            self.index
                .item_by_decl(self.input.file(), Facts::span(new.syntax()).start)
                .ok_or(WasmError::Unsupported("an anonymous class with no item"))?
        } else {
            self.input
                .type_of_expr(Facts::span(new.syntax()))
                .and_then(Ty::project_id)
                .ok_or(WasmError::Unsupported("a `new` of an unindexed type"))?
        };
        // The expression's *inferred* type is the interface the `new` named, which is held as `anyref` —
        // but the value on the stack is the anonymous struct, and anything that writes its fields needs
        // the concrete type. The subtyping makes the two interchangeable everywhere else, which is why
        // only the capture stores noticed.
        let ty = if anonymous {
            self.layout.class_ref(item)?
        } else {
            ty
        };
        let struct_type =
            *self.layout.structs.get(&item).ok_or_else(|| {
                WasmError::NoRepresentation(self.index.item(item).fqn.to_string())
            })?;

        // An inner class's constructor needs the enclosing instance. Only the unqualified form is
        // lowered: `outer.new Inner()` names a *different* enclosing instance, and taking `this`
        // regardless would build the object against the wrong one — wrong state, silently.
        let encloses = self.layout.inner.get(&item).copied();
        // `outer.new Inner()` names the enclosing instance explicitly; the qualifier is an expression
        // sitting *before* the `new` keyword. Unqualified, it is `this`, which a `static` method has not
        // got.
        let qualifier = encloses.and_then(|_| Self::new_qualifier(new));
        if encloses.is_some() && qualifier.is_none() && self.owner.is_none() {
            return Err(WasmError::Unsupported(
                "a `new` of an inner class outside an instance method",
            ));
        }
        let arguments: Vec<ast::Expr> = new
            .syntax()
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        // Which constructor, read from the index rather than re-picked here. Matching on argument
        // *count* alone took the first of any same-arity pair, and a second selection free to
        // disagree with the analysis is the drift `call_target_of` exists to prevent.
        let constructor = self.input.call_target_of(Facts::span(new.syntax()));
        let declares_constructor = self
            .index
            .own_members(item)
            .iter()
            .any(|&member| self.index.member(member).kind == DefKind::Constructor);

        insn.struct_new_default(struct_type);
        match constructor {
            Some(constructor) => {
                let function = *self
                    .layout
                    .functions
                    .get(&constructor)
                    .ok_or(WasmError::Unsupported("a constructor with no body"))?;
                // The receiver has to survive the call, so it is stored and re-read rather than
                // duplicated: wasm has no `dup`. `local.set` and not `local.tee` — a `tee` leaves
                // the value as well, and the copy it left behind outlived the call, so `new`
                // finished one value deep. A trailing `return` discards a surplus, which is why
                // that only surfaced once a `new` sat inside a `block`.
                let slot = self.scratch(ty);
                insn.local_set(slot).local_get(slot);
                // The enclosing instance is the constructor's first declared argument.
                if encloses.is_some() {
                    self.enclosing_instance(qualifier.as_ref(), insn)?;
                }
                for argument in &arguments {
                    self.expr(argument, insn)?;
                }
                self.push_captures(item, insn)?;
                insn.call(function).local_get(slot);
            }
            // No declared constructor: the implicit default one initialises nothing, so the
            // allocation is already the finished object — except for an inner class, whose synthetic
            // field is written here because there is no constructor function to write it.
            None if !declares_constructor && arguments.is_empty() => {
                // No constructor at all, so nothing else will run the field initialisers: without this a
                // `class Box { int value = 9; }` read back as 0 — a wrong value in a module that validates.
                // Its own synthesised constructor, or — when it has no initialisers of its own — the
                // nearest ancestor's, whose initialisers still have to run.
                if let Some(initialise) = self
                    .layout
                    .default_constructors
                    .get(&item)
                    .copied()
                    .or_else(|| Body::super_constructor(item, self.index, self.layout))
                {
                    let slot = self.scratch(ty);
                    insn.local_set(slot).local_get(slot).call(initialise);
                    insn.local_get(slot);
                }
                // No constructor function to fill the capture fields either, so the `new` fills them — the
                // same way it fills an inner class's single enclosing instance. An anonymous class is always
                // this case: it never declares a constructor.
                let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
                if !captured.is_empty() {
                    let slot = self.scratch(ty);
                    let first = *self
                        .layout
                        .capture_slot
                        .get(&item)
                        .ok_or(WasmError::Unsupported("a capture with no field"))?;
                    insn.local_set(slot);
                    for (offset, (id, _)) in captured.iter().enumerate() {
                        insn.local_get(slot);
                        self.push_capture(*id, insn)?;
                        let field =
                            first + u32::try_from(offset).map_err(|_| WasmError::TooLarge)?;
                        insn.struct_set(struct_type, field);
                    }
                    insn.local_get(slot);
                }
                if encloses.is_some() {
                    let slot = self.scratch(ty);
                    let field = self
                        .layout
                        .outer
                        .get(&item)
                        .copied()
                        .ok_or(WasmError::Unsupported("an inner class with no outer field"))?;
                    insn.local_set(slot);
                    insn.local_get(slot);
                    self.enclosing_instance(qualifier.as_ref(), insn)?;
                    insn.struct_set(struct_type, field);
                    insn.local_get(slot);
                }
            }
            None => return Err(WasmError::Unresolved("a matching constructor".into())),
        }
        Ok(ty)
    }

    /// Every class in this module that overrides `member`, most-derived first.
    ///
    /// wasm has no dynamic loading and no classpath: this backend compiles the *whole* project as one
    /// module, so the set of classes that can override a method is closed and known here. That is what
    /// makes dispatch by type test sound — and it is the only reason it is, which is why it is written
    /// down rather than assumed.
    ///
    /// Empty when nothing overrides the method, which is the common case and the one that keeps a
    /// direct `call`.
    fn overriders(&self, member: MemberId) -> Vec<(ItemId, MemberId)> {
        let info = self.index.member(member);
        if info.kind != DefKind::Method {
            return Vec::new();
        }
        let owner = info.owner;
        let mut found: Vec<(ItemId, MemberId)> = Vec::new();
        for &item in self.layout.structs.keys() {
            if item == owner || !self.index.is_subtype(item, owner) {
                continue;
            }
            // Only a definite override. A false positive here routes a call to the wrong method
            // — output that loads, validates, and runs wrongly, which no later stage catches —
            // while a false negative leaves the direct `call` a non-overridden method would have
            // had anyway. That is the opposite collapse from the bridge emission's, and it is why
            // the shared fact has three answers rather than two.
            let over = self
                .index
                .own_members(item)
                .iter()
                .copied()
                .find(|&id| Hierarchy::of(self.index).overrides(id, member) == Overrides::Yes);
            if let Some(over) = over {
                found.push((item, over));
            }
        }
        // Most-derived first, so a subclass's override is tested before its superclass's: testing the
        // other way round would let the base class's `ref.test` succeed for every descendant and answer
        // with the wrong method.
        found.sort_by(|&(a, _), &(b, _)| {
            self.index
                .is_subtype(a, b)
                .cmp(&self.index.is_subtype(b, a))
                .reverse()
        });
        found
    }

    /// A virtual call: test the receiver's actual type against each override, most-derived first, and
    /// fall through to the statically-selected method when none matches.
    ///
    /// The receiver and every argument are spilled into locals first, because each arm re-pushes them
    /// and Java evaluates them exactly once. There is no vtable and no `call_ref`: with the whole
    /// project in one module the overrides are a known, closed set, so a chain of `ref.test` answers
    /// the same question a vtable would — and needs no element section to declare function references
    /// in.
    fn virtual_call(
        &mut self,
        call: &ast::CallExpr,
        member: MemberId,
        arguments: &[ast::Expr],
        overriders: &[(ItemId, MemberId)],
        ty: Option<ValType>,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        // A bare call in an instance method is an implicit `this`, which is local 0.
        let receiver_ty = if let Some(ast::Expr::FieldAccess(access)) = call.callee() {
            let receiver = access
                .receiver()
                .ok_or(WasmError::Unsupported("a call with no receiver"))?;
            self.expr(&receiver, insn)?
                .ok_or(WasmError::Unsupported("a receiver with no value"))?
        } else {
            let owner = self
                .owner
                .ok_or(WasmError::Unsupported("a bare call in a `static` method"))?;
            insn.local_get(0);
            self.layout.class_ref(owner)?
        };
        let receiver = self.scratch(receiver_ty);
        insn.local_set(receiver);

        // Lowered untargeted, exactly as the direct-call path does: an argument's own inferred type is
        // what both use, so a virtual call converts no differently from a static one.
        let mut slots = Vec::with_capacity(arguments.len());
        for argument in arguments {
            let value = self
                .expr(argument, insn)?
                .ok_or(WasmError::Unsupported("an argument with no value"))?;
            let slot = self.scratch(value);
            insn.local_set(slot);
            slots.push(slot);
        }

        match ty {
            Some(ty) => insn.block_typed(ty),
            None => insn.block(),
        };
        let leave = insn.depth();
        for &(item, over) in overriders {
            let Some(&function) = self.layout.functions.get(&over) else {
                continue;
            };
            let struct_type = self.layout.structs[&item];
            insn.local_get(receiver);
            insn.ref_test(HeapType::Concrete(struct_type), false);
            insn.if_();
            insn.local_get(receiver);
            insn.ref_cast(HeapType::Concrete(struct_type), false);
            for &slot in &slots {
                insn.local_get(slot);
            }
            insn.call(function);
            insn.br(insn.depth() - leave);
            insn.end();
        }
        // An interface's method is abstract: there is no function to fall back to, and every class that
        // could satisfy the call is already in the chain above. Reaching here means the receiver is
        // `null` or of a type nothing implemented, which traps — the same answer `ref.cast` gives a
        // failed cast, this host having no exception model.
        match self.layout.functions.get(&member) {
            Some(&function) => {
                insn.local_get(receiver);
                for &slot in &slots {
                    insn.local_get(slot);
                }
                insn.call(function);
            }
            None => {
                insn.unreachable();
            }
        }
        insn.end();
        Ok(ty)
    }

    /// The expression a qualified `new` names as its enclosing instance: the one sitting *before* the
    /// `new` keyword. `None` for the unqualified form.
    fn new_qualifier(new: &ast::NewExpr) -> Option<ast::Expr> {
        let keyword = new
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::NEW_KW)?;
        new.syntax()
            .children()
            .filter(|child| child.text_range().end() <= keyword.text_range().start())
            .find_map(ast::Expr::cast)
    }

    /// Push the enclosing instance an inner class's constructor takes: the qualifier when the source
    /// wrote one, `this` otherwise.
    fn enclosing_instance(&mut self, qualifier: Option<&ast::Expr>, insn: &mut Insn) -> Result<()> {
        match qualifier {
            Some(expr) => {
                self.expr(expr, insn)?
                    .ok_or(WasmError::Unsupported("a qualified `new` with no receiver"))?;
            }
            None => {
                insn.local_get(0);
            }
        }
        Ok(())
    }

    /// Whether a reference names `new` rather than a method.
    fn constructs(node: &SyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == jals_syntax::SyntaxKind::NEW_KW)
    }

    /// The type a `T::new` reference constructs.
    fn constructed_item(
        node: &SyntaxNode,
        input: &TypedFile<'_>,
        index: &ProjectIndex,
    ) -> Result<ItemId> {
        let qualifier = node
            .children()
            .find_map(ast::Expr::cast)
            .ok_or(WasmError::Unsupported(
                "a constructor reference with no type",
            ))?;
        let _ = input;
        index
            .item_by_fqn(qualifier.syntax().text().to_string().trim())
            .ok_or(WasmError::Unsupported(
                "a constructor reference to an unindexed type",
            ))
    }

    /// The `(struct field, type)` a captured local is read through, when `id` is one of the enclosing
    /// class's captures.
    fn capture_field(&self, id: DefId) -> Option<(u32, Ty)> {
        let owner = self.owner?;
        let captured = self.layout.captures.get(&owner)?;
        let first = *self.layout.capture_slot.get(&owner)?;
        let position = captured.iter().position(|(seen, _)| *seen == id)?;
        Some((
            first + u32::try_from(position).ok()?,
            captured[position].1.clone(),
        ))
    }

    /// Push the values a local class's constructor takes for its captures, read from wherever they live
    /// here — a local of the enclosing method, or *this* class's own capture field when one local class
    /// creates another.
    fn push_captures(&self, item: ItemId, insn: &mut Insn) -> Result<()> {
        let captured = self.layout.captures.get(&item).cloned().unwrap_or_default();
        for (id, _) in &captured {
            self.push_capture(*id, insn)?;
        }
        Ok(())
    }

    /// Push one captured local's value, from wherever it lives *here*: a local of the enclosing method, or
    /// this class's own capture field when one capturing class creates another.
    fn push_capture(&self, id: DefId, insn: &mut Insn) -> Result<()> {
        if let Some(slot) = self.slot_of(id) {
            insn.local_get(slot);
        } else if let Some((field, _)) = self.capture_field(id) {
            let owner = self
                .owner
                .ok_or(WasmError::Unsupported("a capture with no enclosing class"))?;
            insn.local_get(0)
                .struct_get(self.layout.structs[&owner], field);
        } else {
            return Err(WasmError::Unsupported("a capture with no value here"));
        }
        Ok(())
    }

    /// A fresh unnamed local of type `ty`, for values that must outlive the stack.
    fn scratch(&mut self, ty: ValType) -> u32 {
        let slot = self.next;
        self.locals.push(ty);
        self.next += 1;
        slot
    }

    fn call(&mut self, call: &ast::CallExpr, insn: &mut Insn) -> Result<Option<ValType>> {
        let member = self
            .input
            .call_target_of(Facts::span(call.syntax()))
            .ok_or_else(|| WasmError::Unresolved(call.syntax().text().to_string().trim().into()))?;
        let info = self.index.member(member);
        let is_static = info.modifiers.is_static;

        let arguments: Vec<ast::Expr> = call.args().into_iter().flat_map(|l| l.args()).collect();
        let overriders = if is_static {
            Vec::new()
        } else {
            self.overriders(member)
        };
        if !overriders.is_empty() {
            let ty = match self.index.resolved_member_ty(member) {
                Ty::Void => None,
                ty => Some(self.layout.val_type(&ty)?),
            };
            return self.virtual_call(call, member, &arguments, &overriders, ty, insn);
        }
        // Only now: a method with no function index is abstract, and an abstract one is only ever
        // reached through the chain above. Looking it up first reported "outside this module" for every
        // interface call, which named the wrong problem.
        let function = *self
            .layout
            .functions
            .get(&member)
            .ok_or(WasmError::Unsupported(
                "a call to a method outside this module",
            ))?;
        if !is_static {
            match call.callee() {
                Some(ast::Expr::FieldAccess(access)) => {
                    let receiver = access
                        .receiver()
                        .ok_or(WasmError::Unsupported("a call with no receiver"))?;
                    self.expr(&receiver, insn)?;
                }
                // A bare call in an instance method is an implicit `this`.
                _ => {
                    insn.local_get(0);
                }
            }
        }
        for argument in &arguments {
            self.expr(argument, insn)?;
        }
        insn.call(function);

        // A constructor has no return type at all — `resolved_member_ty` reports `Unknown` for one,
        // which is not a type this backend could represent even in principle. `this(…)` and `super(…)`
        // are calls to one, and they produce no value.
        if self.index.member(member).kind == DefKind::Constructor {
            return Ok(None);
        }
        match self.index.resolved_member_ty(member) {
            Ty::Void => Ok(None),
            ty => Ok(Some(self.layout.val_type(&ty)?)),
        }
    }
}

/// Where an assignable value lives, once its subexpressions have been evaluated.
///
/// The JVM backend's equivalent duplicates an address under a value with `dup_x1`; wasm has no such
/// instruction, so every operand a store needs is held in a local and pushed again for each access.
/// That is the whole difference between the two protocols: here an address is *re-emitted*, not
/// duplicated.
#[derive(Debug, Clone, Copy)]
enum Place {
    Local {
        slot: u32,
        ty: ValType,
    },
    /// A field of an object whose reference is in local `receiver`.
    Field {
        receiver: u32,
        struct_type: u32,
        slot: u32,
        ty: ValType,
    },
    /// A `static` field, which is a module-level global.
    Global {
        index: u32,
        ty: ValType,
    },
    /// An element of the array in local `array` at the index in local `index`.
    Element {
        array: u32,
        index: u32,
        array_type: u32,
        ty: ValType,
    },
}

impl Place {
    const fn ty(self) -> ValType {
        match self {
            Self::Local { ty, .. }
            | Self::Global { ty, .. }
            | Self::Field { ty, .. }
            | Self::Element { ty, .. } => ty,
        }
    }

    /// Push the operands [`store`](Self::store) needs *below* the value.
    fn address(self, insn: &mut Insn) {
        match self {
            Self::Local { .. } | Self::Global { .. } => {}
            Self::Field { receiver, .. } => {
                insn.local_get(receiver);
            }
            Self::Element { array, index, .. } => {
                insn.local_get(array).local_get(index);
            }
        }
    }

    /// Push the value currently held here.
    fn read(self, insn: &mut Insn) {
        match self {
            Self::Local { slot, .. } => {
                insn.local_get(slot);
            }
            Self::Global { index, .. } => {
                insn.global_get(index);
            }
            Self::Field {
                receiver,
                struct_type,
                slot,
                ..
            } => {
                insn.local_get(receiver).struct_get(struct_type, slot);
            }
            Self::Element {
                array,
                index,
                array_type,
                ..
            } => {
                insn.local_get(array).local_get(index).array_get(array_type);
            }
        }
    }

    /// Consume the value on top of the stack, storing it here. `keep` leaves it behind.
    ///
    /// wasm has no `dup`, so nothing can duplicate a value under a `struct.set`: keeping one means
    /// `local.tee` for a local, and for a field or an element a second load of what was just written —
    /// which is the same value, this backend having no volatile fields and no threads.
    fn store(self, insn: &mut Insn, keep: bool) {
        match self {
            Self::Local { slot, .. } => {
                if keep {
                    insn.local_tee(slot);
                } else {
                    insn.local_set(slot);
                }
            }
            // No `global.tee`, so keeping the value is a read back — the same value, this backend
            // having neither threads nor volatile fields.
            Self::Global { index, .. } => {
                insn.global_set(index);
                if keep {
                    self.read(insn);
                }
            }
            Self::Field {
                struct_type, slot, ..
            } => {
                insn.struct_set(struct_type, slot);
                if keep {
                    self.read(insn);
                }
            }
            Self::Element { array_type, .. } => {
                insn.array_set(array_type);
                if keep {
                    self.read(insn);
                }
            }
        }
    }
}
