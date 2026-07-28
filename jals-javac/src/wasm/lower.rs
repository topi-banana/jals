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
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

use jals_hir::{
    DefId, DefKind, FileId, ItemId, MemberId, Primitive, ProjectIndex, Resolved, Ty, TypeInference,
};
use jals_syntax::SyntaxKind::{CLASS_DECL, CONSTRUCTOR_DECL, METHOD_DECL};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::wasm::encode::{
    CompType, ExportKind, FieldType, Func, HeapType, Module, RefType, StorageType, SubType, ValType,
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

type Result<T> = core::result::Result<T, WasmError>;

/// One parsed source and its analyses.
pub struct WasmInput<'a> {
    pub file: FileId,
    pub root: &'a SyntaxNode,
    pub resolved: &'a Resolved,
    pub inference: &'a TypeInference,
}

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
}

/// Compiles a whole project to one WebAssembly module.
pub struct CompileWasm;

impl CompileWasm {
    /// Emit the module. `index` must have been built over exactly `inputs`.
    pub fn project(inputs: &[WasmInput<'_>], index: &ProjectIndex) -> Result<Vec<u8>> {
        let mut module = Module::new();
        let mut layout = Layout::default();

        // Pass 1: every class gets a struct type, in an order where a supertype is declared first
        // so its field prefix is known when the subtype is laid out.
        for item in Self::classes_in_order(inputs, index)? {
            layout.declare_class(item, index, &mut module)?;
        }
        // Then every array type the program mentions. wasm has one declared type per element
        // type, and a body cannot introduce one mid-lowering, so they are collected from the
        // types the analyses already recorded rather than discovered while emitting.
        for input in inputs {
            for node in input.root.descendants() {
                if let Some(ty) = input.inference.type_of_expr(Lowering::span(&node)) {
                    layout.declare_array(ty, &mut module)?;
                }
            }
            for def in &input.resolved.defs {
                let ty = input.inference.type_of_def(def.id).clone();
                layout.declare_array(&ty, &mut module)?;
            }
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
        module.finish().ok_or(WasmError::TooLarge)
    }

    /// Every project class, supertypes first.
    ///
    /// A struct's fields start with its supertype's, so the supertype's layout has to be settled
    /// first. The order is a depth-first walk of the `extends` chain; a cycle is impossible in a
    /// well-formed program and is simply not revisited here.
    fn classes_in_order(inputs: &[WasmInput<'_>], index: &ProjectIndex) -> Result<Vec<ItemId>> {
        let mut declared = Vec::new();
        for input in inputs {
            for node in input.root.children().filter(|n| n.kind() == CLASS_DECL) {
                let name = Self::name_token(&node)
                    .ok_or(WasmError::Unsupported("a class with no name"))?;
                let item = index
                    .item_by_decl(input.file, usize::from(name.text_range().start()))
                    .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
                declared.push(item);
            }
        }

        let mut ordered: Vec<ItemId> = Vec::with_capacity(declared.len());
        for &item in &declared {
            Self::push_with_supertypes(item, index, &declared, &mut ordered);
        }
        Ok(ordered)
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
            .find(|&id| index.item(id).kind == DefKind::Class)
    }

    /// Register every method and constructor `input` declares.
    fn collect_methods(
        input: &WasmInput<'_>,
        position: usize,
        index: &ProjectIndex,
        layout: &mut Layout,
        module: &mut Module,
        out: &mut Vec<Method>,
    ) -> Result<()> {
        for class in input.root.children().filter(|n| n.kind() == CLASS_DECL) {
            let name =
                Self::name_token(&class).ok_or(WasmError::Unsupported("a class with no name"))?;
            let item = index
                .item_by_decl(input.file, usize::from(name.text_range().start()))
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let Some(body) = class.children().find_map(ast::ClassBody::cast) else {
                continue;
            };
            for node in body.syntax().children() {
                if !matches!(node.kind(), METHOD_DECL | CONSTRUCTOR_DECL) {
                    continue;
                }
                let is_constructor = node.kind() == CONSTRUCTOR_DECL;
                let member_name = Self::name_token(&node)
                    .ok_or(WasmError::Unsupported("a member with no name"))?;
                let member = index
                    .member_by_decl(input.file, usize::from(member_name.text_range().start()))
                    .ok_or_else(|| WasmError::Unresolved(member_name.text().into()))?;
                let is_static = index.member(member).modifiers.is_static;

                let mut params = Vec::new();
                if is_constructor || !is_static {
                    params.push(layout.class_ref(item)?);
                }
                for ty in index.resolved_param_tys(member) {
                    params.push(layout.val_type(&ty)?);
                }
                let results = if is_constructor {
                    Vec::new()
                } else {
                    match index.resolved_member_ty(member) {
                        Ty::Void => Vec::new(),
                        ty => alloc::vec![layout.val_type(&ty)?],
                    }
                };

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
                    export: (is_static && !is_constructor)
                        .then(|| index.member(member).name.clone()),
                    is_constructor,
                });
            }
        }
        Ok(())
    }

    fn name_token(node: &SyntaxNode) -> Option<SyntaxToken> {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
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
    /// `(element type, array type index)`. A `Vec` because `ValType` has no ordering and a program
    /// has a handful of distinct element types.
    arrays: Vec<(ValType, u32)>,
}

impl Layout {
    /// Give `item` a struct type whose fields are its supertype's followed by its own.
    fn declare_class(
        &mut self,
        item: ItemId,
        index: &ProjectIndex,
        module: &mut Module,
    ) -> Result<()> {
        if self.structs.contains_key(&item) {
            return Ok(());
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

        let mut fields = Vec::with_capacity(members.len());
        for &member in &members {
            fields.push(FieldType {
                storage: StorageType::Val(self.val_type(&index.resolved_member_ty(member))?),
                // Every Java field is assignable unless `final`, and even a `final` one is written
                // once by a constructor — after `struct.new_default` has already made it.
                mutable: true,
            });
        }

        let type_index = module.add_type(SubType {
            is_final: false,
            supertype: parent.and_then(|id| self.structs.get(&id)).copied(),
            comp: CompType::Struct(fields),
        });
        self.structs.insert(item, type_index);
        self.fields.insert(item, members);
        Ok(())
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
                    .filter(|id| self.structs.contains_key(id))
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

/// One method body being lowered.
struct Body {
    locals: Vec<ValType>,
    code: Vec<u8>,
}

impl Body {
    fn lower(
        method: &Method,
        input: &WasmInput<'_>,
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
        };
        // `this` is parameter 0 of an instance method or a constructor.
        if method.owner.is_some() || method.is_constructor {
            lowering.next += 1;
        }
        if let Some(params) = method.node.children().find_map(ast::ParamList::cast) {
            for param in params.params() {
                let ty = lowering.declare_param(param.syntax())?;
                let _ = ty;
            }
        }

        let mut insn = Insn::new();
        if let Some(block) = method.node.children().find_map(ast::Block::cast) {
            lowering.block(&block, &mut insn)?;
        }
        Ok(Self {
            locals: lowering.locals,
            code: insn.into_body(),
        })
    }
}

/// The mutable state of lowering one body.
struct Lowering<'a> {
    input: &'a WasmInput<'a>,
    index: &'a ProjectIndex,
    layout: &'a Layout,
    /// `(definition, local index)` pairs, parameters first.
    slots: Vec<(DefId, u32)>,
    /// Locals beyond the parameters, in declaration order.
    locals: Vec<ValType>,
    /// The next free local index. Unlike the JVM's, a wasm local is one slot whatever its width.
    next: u32,
    owner: Option<ItemId>,
}

impl Lowering<'_> {
    fn declare_param(&mut self, node: &SyntaxNode) -> Result<ValType> {
        let id = self
            .def_at(node)
            .ok_or(WasmError::Unsupported("an unresolved parameter"))?;
        let ty = self.layout.val_type(self.input.inference.type_of_def(id))?;
        self.slots.push((id, self.next));
        self.next += 1;
        Ok(ty)
    }

    fn declare_local(&mut self, id: DefId) -> Result<u32> {
        let ty = self.layout.val_type(self.input.inference.type_of_def(id))?;
        let slot = self.next;
        self.slots.push((id, slot));
        self.locals.push(ty);
        self.next += 1;
        Ok(slot)
    }

    fn slot_of(&self, id: DefId) -> Option<u32> {
        self.slots
            .iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, slot)| *slot)
    }

    fn def_at(&self, node: &SyntaxNode) -> Option<DefId> {
        let token = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)?;
        let start = usize::from(token.text_range().start());
        self.input
            .resolved
            .reference_at(start)
            .and_then(|reference| reference.resolution.def_id())
            .or_else(|| self.input.resolved.symbol_at(start))
    }

    fn span(node: &SyntaxNode) -> core::ops::Range<usize> {
        let range = node.text_range();
        usize::from(range.start())..usize::from(range.end())
    }

    fn ty_of(&self, node: &SyntaxNode) -> Result<ValType> {
        let ty =
            self.input
                .inference
                .type_of_expr(Self::span(node))
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
            ast::Stmt::Empty(_) => Ok(()),
            ast::Stmt::LocalVar(declaration) => self.local(declaration, insn),
            ast::Stmt::Expr(expression) => {
                let Some(value) = expression.expr() else {
                    return Ok(());
                };
                let dropped = self.expr(&value, insn)?;
                if dropped.is_some() {
                    insn.drop();
                }
                Ok(())
            }
            ast::Stmt::Return(statement) => {
                if let Some(value) = statement.expr() {
                    self.expr(&value, insn)?;
                }
                insn.return_();
                Ok(())
            }
            ast::Stmt::If(statement) => self.conditional(statement, insn),
            ast::Stmt::While(statement) => self.while_loop(statement, insn),
            _ => Err(WasmError::Unsupported("this statement form")),
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
                .resolved
                .symbol_at(usize::from(name.text_range().start()))
                .ok_or_else(|| WasmError::Unresolved(name.text().into()))?;
            let slot = self.declare_local(id)?;
            if let Some(value) = values.get(position) {
                self.expr(value, insn)?;
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

    /// `while` is a `block` around a `loop`: `br 1` leaves, `br 0` repeats. The two labels are why
    /// wasm needs both instructions — a `loop` alone can only jump backwards.
    fn while_loop(&mut self, statement: &ast::WhileStmt, insn: &mut Insn) -> Result<()> {
        let condition = statement
            .condition()
            .ok_or(WasmError::Unsupported("a `while` with no condition"))?;
        insn.block().loop_();
        self.expr(&condition, insn)?;
        // Leave when the condition is false: negate, then branch out of both structures.
        insn.i32_eqz().br_if(1);
        if let Some(body) = statement.body() {
            self.stmt(&body, insn)?;
        }
        insn.br(0).end().end();
        Ok(())
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
            ast::Expr::NameRef(_) if Self::is_this(expr.syntax()) => {
                let owner = self
                    .owner
                    .ok_or(WasmError::Unsupported("`this` in a `static` method"))?;
                insn.local_get(0);
                self.layout.class_ref(owner).map(Some)
            }
            ast::Expr::NameRef(name) => self.name(name, insn).map(Some),
            ast::Expr::Binary(binary) => self.binary(binary, insn).map(Some),
            ast::Expr::Assignment(assignment) => self.assignment(assignment, insn),
            ast::Expr::New(new) => self.new_object(new, insn).map(Some),
            ast::Expr::Call(call) => self.call(call, insn),
            ast::Expr::Index(index) => self.index(index, insn).map(Some),
            ast::Expr::FieldAccess(access) => self.field(access, insn).map(Some),
            ast::Expr::Unary(unary) => self.unary(unary, insn).map(Some),
            ast::Expr::Cast(cast) => self.cast(cast, insn).map(Some),
            _ => Err(WasmError::Unsupported("this expression form")),
        }
    }

    /// Whether `node` is the `this` expression. It carries no identifier token, so nothing
    /// resolves it as a name; its keyword is the only thing that identifies it.
    fn is_this(node: &SyntaxNode) -> bool {
        node.children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == jals_syntax::SyntaxKind::THIS_KW)
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
            Some(receiver) if Self::is_this(receiver.syntax()) => {
                let owner = self.owner.ok_or_else(unresolved)?;
                self.index
                    .resolve_member(owner, &name, jals_hir::Namespace::Value)
                    .ok_or_else(unresolved)?
            }
            _ => self
                .input
                .inference
                .field_target_of(Self::span(access.syntax()))
                .ok_or_else(unresolved)?,
        };
        Ok((self.index.member(member).owner, member))
    }

    fn literal(&self, literal: &ast::Literal, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{FALSE_KW, FLOAT_LITERAL, INT_LITERAL, NULL_KW, TRUE_KW};
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
                // Read by the same routine the JVM backend uses: `0xFF`, `0b1010`, `017`, and `1_000`
                // all mean what they mean in both, and reading them twice would be two chances to
                // disagree about one of them.
                let value =
                    crate::lower::expr::Expr::integer_literal(text.trim_end_matches(['l', 'L']))
                        .map_err(|_| {
                            WasmError::Unsupported("an integer literal this cannot read")
                        })?;
                match ty {
                    ValType::I64 => insn.i64_const(value),
                    _ => insn
                        .i32_const(i32::try_from(value).map_err(|_| {
                            WasmError::Unsupported("an out-of-range `int` literal")
                        })?),
                };
            }
            FLOAT_LITERAL => {
                let text = text.trim_end_matches(['f', 'F', 'd', 'D']);
                let unreadable = || WasmError::Unsupported("a floating literal this cannot read");
                match ty {
                    ValType::F32 => insn.f32_const(text.parse().map_err(|_| unreadable())?),
                    _ => insn.f64_const(text.parse().map_err(|_| unreadable())?),
                };
            }
            _ => return Err(WasmError::Unsupported("this literal kind")),
        }
        Ok(ty)
    }

    fn name(&self, name: &ast::NameRef, insn: &mut Insn) -> Result<ValType> {
        let text = name.syntax().text().to_string();
        let unresolved = || WasmError::Unresolved(text.trim().into());
        let id = self.def_at(name.syntax()).ok_or_else(unresolved)?;
        if let Some(slot) = self.slot_of(id) {
            insn.local_get(slot);
            return self.layout.val_type(self.input.inference.type_of_def(id));
        }
        // Not a local: an unqualified field of the enclosing class, reached through `this`.
        let owner = self.owner.ok_or_else(unresolved)?;
        let declaration = self.input.resolved.def(id);
        let member = self
            .index
            .member_by_decl(self.input.file, declaration.name_range.start)
            .ok_or_else(unresolved)?;
        let slot = self
            .layout
            .field_slot(owner, member)
            .ok_or_else(unresolved)?;
        let struct_type = self.layout.structs[&owner];
        insn.local_get(0).struct_get(struct_type, slot);
        self.layout.val_type(&self.index.resolved_member_ty(member))
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
                self.input
                    .inference
                    .type_of_expr(Self::span(receiver.syntax())),
                Some(Ty::Array(_))
            )
        {
            self.expr(&receiver, insn)?;
            insn.array_len();
            return Ok(ValType::I32);
        }
        let (owner, member) = self.field_target(access)?;
        if self.index.member(member).modifiers.is_static {
            return Err(WasmError::Unsupported("a `static` field"));
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

    fn unary(&mut self, unary: &ast::UnaryExpr, insn: &mut Insn) -> Result<ValType> {
        use jals_syntax::SyntaxKind::{BANG, MINUS, PLUS, TILDE};
        let operand = unary
            .operand()
            .ok_or(WasmError::Unsupported("a unary with no operand"))?;
        let operator: Vec<_> = unary
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect();
        match operator.as_slice() {
            // `!b` flips a `boolean`, which is an `i32` that is 0 or 1 — so `i32.eqz` *is* the flip.
            [BANG] => {
                self.expr(&operand, insn)?
                    .ok_or(WasmError::Unsupported("a `!` on nothing"))?;
                insn.i32_eqz();
                Ok(ValType::I32)
            }
            // `+` is not a no-op: unary numeric promotion still applies.
            [PLUS] => {
                let promoted = Self::promote_one(self.num_of(operand.syntax())?);
                self.operand(&operand, promoted, insn)?;
                Ok(promoted.val())
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
                Ok(promoted.val())
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
                Ok(promoted.val())
            }
            _ => Err(WasmError::Unsupported("this unary operator")),
        }
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
            AMP, BANG_EQ, CARET, EQ, EQ_EQ, GT, INSTANCEOF_KW, LSHIFT, LT, LT_EQ, MINUS, PERCENT,
            PIPE, PLUS, SLASH, STAR,
        };
        let left = binary
            .lhs()
            .ok_or(WasmError::Unsupported("a binary with no left operand"))?;
        let right = binary
            .rhs()
            .ok_or(WasmError::Unsupported("a binary with no right operand"))?;
        let operator: Vec<_> = binary
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .map(|token| token.kind())
            .filter(|kind| !kind.is_trivia())
            .collect();

        if operator.first() == Some(&INSTANCEOF_KW) {
            return self.instance_of(binary, insn);
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
            .inference
            .type_of_expr(Self::span(node))
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
            self.input.inference.type_of_expr(Self::span(node)),
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
        let operand = binary
            .lhs()
            .ok_or(WasmError::Unsupported("an `instanceof` with no operand"))?;
        let ty = binary
            .syntax()
            .children()
            .find_map(ast::Type::cast)
            .ok_or(WasmError::Unsupported("an `instanceof` with no type"))?;
        let target = self.named_type(&ty)?;
        self.expr(&operand, insn)?
            .ok_or(WasmError::Unsupported("an `instanceof` on nothing"))?;
        insn.ref_test(target, false);
        Ok(ValType::I32)
    }

    /// The declared heap type a `TYPE` node names.
    fn named_type(&self, ty: &ast::Type) -> Result<HeapType> {
        let name = ty
            .simple_name()
            .ok_or(WasmError::Unsupported("a type with no name"))?;
        let qualified = ty.is_qualified().then(|| ty.qualified_text()).flatten();
        let item = self
            .index
            .resolve_type_name(self.input.file, &name, qualified.as_deref())
            .project_id()
            .ok_or_else(|| WasmError::Unresolved(name.clone()))?;
        let index = self
            .layout
            .structs
            .get(&item)
            .ok_or(WasmError::NoRepresentation(name))?;
        Ok(HeapType::Concrete(*index))
    }

    /// An assignment. Returns the assigned type when the value survives on the stack, and `None`
    /// when it does not: `struct.set` consumes both operands, so a field assignment is a statement
    /// unless the value is duplicated, which nothing needs yet.
    fn assignment(
        &mut self,
        assignment: &ast::AssignmentExpr,
        insn: &mut Insn,
    ) -> Result<Option<ValType>> {
        // `x = v` and `x += v` share a node kind, so the operator has to be read: a compound
        // assignment reads the target and applies an operator before storing, which this does not
        // lower. Treating it as a plain store would silently compute the wrong value.
        if !assignment.is_simple() {
            return Err(WasmError::Unsupported("a compound assignment"));
        }
        let target = assignment
            .target()
            .ok_or(WasmError::Unsupported("an assignment with no target"))?;
        let value = assignment
            .value()
            .ok_or(WasmError::Unsupported("an assignment with no value"))?;
        if let ast::Expr::FieldAccess(access) = &target {
            self.assign_field(access, &value, insn)?;
            return Ok(None);
        }
        if let ast::Expr::Index(subscript) = &target {
            self.assign_element(subscript, &value, insn)?;
            return Ok(None);
        }
        let ast::Expr::NameRef(name) = &target else {
            return Err(WasmError::Unsupported("assignment to this target"));
        };
        let text = name.syntax().text().to_string();
        let unresolved = || WasmError::Unresolved(text.trim().into());
        let id = self.def_at(name.syntax()).ok_or_else(unresolved)?;

        if let Some(slot) = self.slot_of(id) {
            let ty = self.expr(&value, insn)?.ok_or_else(unresolved)?;
            // `local.tee` writes *and* leaves the value, which is what makes an assignment an
            // expression without a second instruction.
            insn.local_tee(slot);
            return Ok(Some(ty));
        }

        let owner = self.owner.ok_or_else(unresolved)?;
        let declaration = self.input.resolved.def(id);
        let member = self
            .index
            .member_by_decl(self.input.file, declaration.name_range.start)
            .ok_or_else(unresolved)?;
        let slot = self
            .layout
            .field_slot(owner, member)
            .ok_or_else(unresolved)?;
        let struct_type = self.layout.structs[&owner];
        insn.local_get(0);
        self.expr(&value, insn)?.ok_or_else(unresolved)?;
        insn.struct_set(struct_type, slot);
        Ok(None)
    }

    /// `receiver.field = value`: the receiver goes below the value, and `struct.set` takes both.
    fn assign_field(
        &mut self,
        access: &ast::FieldAccess,
        value: &ast::Expr,
        insn: &mut Insn,
    ) -> Result<()> {
        let (owner, member) = self.field_target(access)?;
        if self.index.member(member).modifiers.is_static {
            return Err(WasmError::Unsupported("assignment to a `static` field"));
        }
        let receiver = access.receiver().ok_or(WasmError::Unsupported(
            "a field assignment with no receiver",
        ))?;
        let slot = self
            .layout
            .field_slot(owner, member)
            .ok_or_else(|| WasmError::Unresolved(access.field().unwrap_or_default()))?;

        self.expr(&receiver, insn)?;
        self.expr(value, insn)?
            .ok_or(WasmError::Unsupported("an assignment of no value"))?;
        insn.struct_set(self.layout.structs[&owner], slot);
        Ok(())
    }

    /// `array[index] = value`.
    fn assign_element(
        &mut self,
        expr: &ast::IndexExpr,
        value: &ast::Expr,
        insn: &mut Insn,
    ) -> Result<()> {
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
        self.expr(value, insn)?;
        insn.array_set(array_type);
        Ok(())
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
        if let Some(Ty::Array(element)) =
            self.input.inference.type_of_expr(Self::span(new.syntax()))
        {
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
        let item = self
            .input
            .inference
            .type_of_expr(Self::span(new.syntax()))
            .and_then(Ty::project_id)
            .ok_or(WasmError::Unsupported("a `new` of an unindexed type"))?;
        let struct_type =
            *self.layout.structs.get(&item).ok_or_else(|| {
                WasmError::NoRepresentation(self.index.item(item).fqn.to_string())
            })?;

        let arguments: Vec<ast::Expr> = new
            .syntax()
            .children()
            .find_map(ast::ArgList::cast)
            .map(|list| list.args().collect())
            .unwrap_or_default();
        // Which constructor, read from the index rather than re-picked here. Matching on argument
        // *count* alone took the first of any same-arity pair, and a second selection free to
        // disagree with the analysis is the drift `call_target_of` exists to prevent.
        let constructor = self
            .input
            .inference
            .call_target_of(Self::span(new.syntax()));
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
                for argument in &arguments {
                    self.expr(argument, insn)?;
                }
                insn.call(function).local_get(slot);
            }
            // No declared constructor: the implicit default one initialises nothing, so the
            // allocation is already the finished object.
            None if !declares_constructor && arguments.is_empty() => {}
            None => return Err(WasmError::Unresolved("a matching constructor".into())),
        }
        Ok(ty)
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
            .inference
            .call_target_of(Self::span(call.syntax()))
            .ok_or_else(|| WasmError::Unresolved(call.syntax().text().to_string().trim().into()))?;
        let function = *self
            .layout
            .functions
            .get(&member)
            .ok_or(WasmError::Unsupported(
                "a call to a method outside this module",
            ))?;
        let info = self.index.member(member);
        let is_static = info.modifiers.is_static;

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
        for argument in call.args().into_iter().flat_map(|list| list.args()) {
            self.expr(&argument, insn)?;
        }
        insn.call(function);

        match self.index.resolved_member_ty(member) {
            Ty::Void => Ok(None),
            ty => Ok(Some(self.layout.val_type(&ty)?)),
        }
    }
}
