//! Java source to class files: the lowering that reads a parsed file plus its semantic index and
//! drives the [assembler](crate::jvm::Assembler).
//!
//! # It resolves, it does not check
//!
//! Every semantic question this asks has already been answered. Which overload a call selected,
//! whether a member is `static`, what a name binds to — all of it is read from
//! [`jals_hir`](jals_hir) rather than recomputed, because a second answer would be free to disagree
//! with the first and the disagreement would surface as a `NoSuchMethodError` at run time. What is
//! *not* asked is whether the program is correct: a construct this layer cannot lower is reported
//! as [`LowerError`], never as a type error.
//!
//! # Scope
//!
//! Classes and interfaces with fields and methods; the whole expression grammar bar lambdas —
//! arithmetic with Java's numeric promotions, bitwise and shift operators, comparisons at every
//! width, casts, `instanceof`, the conditional operator, the short-circuiting `&&` / `||`, assignment
//! (simple and compound) to a local, a field, or an array element, `++` / `--` in both positions,
//! object creation, every array-creation form, and string concatenation; `if`, `while`, `do`-`while`,
//! `for`, `for`-each over an array or an `Iterable`, `break` and `continue` with or without a label;
//! and calls (`static`, virtual, and interface). A constructor runs `super()` and the instance
//! initialisers; a `<clinit>` runs the `static` ones.
//!
//! Exceptions too: `throw`, `try` / `catch` (including a multi-catch), `finally`,
//! try-with-resources, `synchronized`, and `assert` — the last guarded by the synthetic
//! `$assertionsDisabled` field, because assertions are off unless the JVM was started with `-ea`.
//!
//! `switch` too, in both syntaxes and as a statement or an expression, over an integral selector or a
//! `String` — the latter through `hashCode()` plus an `equals` per candidate, because two different
//! strings can hash alike.
//!
//! Every statement form in the grammar is now lowered. What a statement still cannot reach it reports
//! from inside: a `case` label with no constant value, a `case` *pattern*, a resource with no
//! `close()`, a local type declaration.
//!
//! A constructor may delegate (`this(…)` / `super(args)`), and a `native` method declares why it has
//! no body with its own flag rather than borrowing `abstract`'s.
//!
//! Boxing and unboxing too, and `.class` literals — including the primitive form, which reads the
//! `TYPE` field its wrapper carries because a primitive has no `Class` entry to `ldc`. A generic call's
//! erased return gets the `checkcast` that puts its static type back, which is what lets the next use
//! of the value verify.
//!
//! A `static` nested type is its own class file, named `Outer$Inner`, listed in an `InnerClasses`
//! attribute — the only place a nested type's `private` and `static` can live — and reachable by simple
//! or partly-qualified name. An `implements` clause reaches the `interfaces` list.
//!
//! An `enum` gets the four member groups its source never writes: a field per constant, the `$VALUES`
//! array, a `(String, int)` constructor reaching `Enum`'s, and `values()` / `valueOf()`.
//!
//! Not yet at all: varargs, `Signature` attributes and bridge methods, lambdas, method references,
//! non-`static` inner classes, local and anonymous classes, and `record` / `@interface` declarations.
//! Each arrives with the milestone that can test it.

mod emit;
// The wasm backend reads a literal the same way: the two lowerings are separate, but `0xFF` and
// `1_000` mean the same thing in both, and reading them twice would be two chances to disagree.
pub(crate) mod expr;
mod place;
mod slots;
mod stmt;
mod switch;

pub(crate) use crate::lower::emit::Emit;

use alloc::borrow::ToOwned as _;
use alloc::string::{String, ToString as _};
use alloc::vec::Vec;

use jals_classfile::{
    ClassAccessFlags, ClassFile, ConstantPool, FieldAccessFlags, FieldInfo, MethodAccessFlags,
    MethodDescriptor, MethodInfo, VerificationType,
};
use jals_hir::{DefKind, FileId, ItemId, ProjectIndex, Resolved, TypeInference};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_DECL, FIELD_DECL,
    INTERFACE_DECL, METHOD_DECL, RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::desc::{DescError, Descriptor};
use crate::jvm::{AsmError, Assembler, BinOp, Branch, Compare, Numeric, Receiver};
use crate::lower::slots::Slots;

/// The supertype every `enum` has, and which its source never writes.
const ENUM: &str = "java/lang/Enum";
/// Every `record`'s superclass, which the source never writes.
const RECORD: &str = "java/lang/Record";
/// The synthetic field an inner class holds its enclosing instance in, named as javac names it.
const OUTER: &str = "this$0";
/// The type of an `enum` constructor's first synthetic parameter, and of `Enum`'s own.
const STRING: &str = "java/lang/String";
/// `Enum`'s constructor, which every `enum` constructor delegates to: the constant's name, then its
/// ordinal. Nothing else can set them, and `name()`, `ordinal()`, and `compareTo` all read them.
const ENUM_INIT: &str = "(Ljava/lang/String;I)V";

/// The synthetic array holding an `enum`'s constants, in declaration order.
///
/// javac's own name. `values()` hands out a *clone* of it, which is why the array can be `final` and
/// still not let a caller reorder the constants.
const VALUES: &str = "$VALUES";

/// One emitted class file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledClass {
    /// The type's internal binary name (`com/example/Main`), which is also its path stem.
    pub internal_name: String,
    /// The class file's bytes.
    pub bytes: Vec<u8>,
}

/// Why a source file could not be lowered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LowerError {
    /// A construct this milestone does not emit yet.
    Unsupported(&'static str),
    /// A name, call, or member access the index did not resolve. Not a diagnostic: the linter
    /// reports unresolved names, and reaching here means the compiler was handed a program the
    /// analysis could not fully index.
    Unresolved(String),
    /// A type could not be turned into a descriptor.
    Descriptor(DescError),
    /// The assembler rejected an emission.
    Assembly(AsmError),
}

impl From<DescError> for LowerError {
    fn from(error: DescError) -> Self {
        Self::Descriptor(error)
    }
}

impl From<AsmError> for LowerError {
    fn from(error: AsmError) -> Self {
        Self::Assembly(error)
    }
}

impl core::fmt::Display for LowerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unsupported(what) => write!(f, "{what} is not compiled yet"),
            Self::Unresolved(name) => write!(f, "`{name}` did not resolve to an indexed member"),
            Self::Descriptor(error) => write!(f, "{error}"),
            Self::Assembly(error) => write!(f, "{error}"),
        }
    }
}

impl core::error::Error for LowerError {}

pub(crate) type Result<T> = core::result::Result<T, LowerError>;

/// Everything a body needs to read while it is being lowered. Immutable throughout: the mutable
/// state is the assembler and the slot map, which are threaded separately.
pub(crate) struct Context<'a> {
    index: &'a ProjectIndex,
    inference: &'a TypeInference,
    resolved: &'a Resolved,
    file: FileId,
    /// The internal name of the type being emitted, for a `this`-qualified access.
    this_class: String,
    /// Whether the type being emitted is an interface, which decides the access levels JLS §9.3
    /// and §9.4 let a declaration leave unwritten.
    in_interface: bool,
    /// Whether the type being emitted is an `enum`, whose every constructor takes two parameters the
    /// source never writes: the constant's name and its ordinal, which are what `Enum`'s own
    /// constructor needs and the only way `name()`, `ordinal()`, and `compareTo` get their answers.
    in_enum: bool,
    /// The internal name of the class an inner class holds an instance of. `None` for every other class.
    encloses: Option<String>,
    /// Every inner class in this file and the class it holds an instance of, so a `new` of one can pass
    /// the enclosing instance even when the creation is not inside the inner class itself.
    inner: alloc::collections::BTreeMap<ItemId, String>,
    /// Every local class in this file and the locals it captures, in source order. A `new` of one reads
    /// this to pass their values; the class itself reads it to turn a capture into a field.
    captures: alloc::collections::BTreeMap<ItemId, alloc::vec::Vec<jals_hir::DefId>>,
    /// The class being compiled, so its own captures can be told from another's.
    this_item: ItemId,
    /// Every lambda in this class, by its span: the call site's name and descriptor, and which
    /// `BootstrapMethods` entry links it.
    lambdas: alloc::collections::BTreeMap<(usize, usize), Lambda>,
}

/// What a lambda's `invokedynamic` needs, worked out before any body is lowered.
///
/// A body cannot turn itself into a method — expression lowering has no channel for adding one — so every
/// lambda is found, numbered, and synthesised up front, exactly as nested classes and captures already are.
/// Expression lowering then only reads this.
pub(crate) struct Lambda {
    /// The functional interface's method name, which is the call site's name.
    interface_method: String,
    /// The call site's descriptor: no arguments, since nothing is captured, returning the interface.
    call_descriptor: String,
    /// Index into the class's `BootstrapMethods`.
    bootstrap: u16,
    /// The locals the call site passes, in the order the descriptor names them.
    captured: alloc::vec::Vec<jals_hir::DefId>,
}

impl Lambda {
    /// The call site's name, which is the functional interface's method name.
    pub(crate) fn interface_method(&self) -> &str {
        &self.interface_method
    }

    /// The call site's descriptor.
    pub(crate) fn call_descriptor(&self) -> &str {
        &self.call_descriptor
    }

    /// Which `BootstrapMethods` entry links this call site.
    pub(crate) const fn bootstrap(&self) -> u16 {
        self.bootstrap
    }

    /// The locals the call site passes, in descriptor order.
    pub(crate) fn captured(&self) -> &[jals_hir::DefId] {
        &self.captured
    }
}

/// The source-to-class-file lowering.
pub struct Compile;

impl Compile {
    /// Compile every top-level type declared in `root` into a class file at `class_version`.
    ///
    /// One class file per declared type is the JVM's own unit, so this returns a list rather than a
    /// single file even for a source file that declares one type — a nested or secondary type is
    /// the same shape of output, arriving with the milestone that emits it.
    pub fn file(
        root: &SyntaxNode,
        resolved: &Resolved,
        inference: &TypeInference,
        index: &ProjectIndex,
        file: FileId,
        class_version: u16,
    ) -> Result<Vec<CompiledClass>> {
        let mut out = Vec::new();
        // `descendants`, not `children`: a nested type is its own class file, so it is compiled here
        // rather than inside its enclosing one. Which is also why the enclosing class skips it as a
        // member — it would otherwise be emitted twice.
        for node in root.descendants() {
            if !matches!(
                node.kind(),
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | ANNOTATION_TYPE_DECL | RECORD_DECL
            ) {
                // An anonymous class body is its own class file too, and the index gave it an item keyed
                // on the `new` keyword's position — which is the only offset it has to be found by.
                if node.kind() == jals_syntax::SyntaxKind::NEW_EXPR
                    && node.children().any(|child| child.kind() == CLASS_BODY)
                    && let Some(item) =
                        index.item_by_decl(file, usize::from(node.text_range().start()))
                {
                    out.push(Self::class(
                        &node,
                        item,
                        resolved,
                        inference,
                        index,
                        file,
                        class_version,
                    )?);
                }
                continue;
            }
            // A *local* class is nested inside a method body rather than a class body, and it is its
            // own class file like any other. What it may not do is capture a local: each capture needs a
            // synthetic constructor parameter the index knows nothing about, so its constructor would
            // come out one parameter short of what a `new` passes.

            let name = node
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
                .ok_or(LowerError::Unsupported("a type declaration with no name"))?;
            let item = index
                .item_by_decl(file, usize::from(name.text_range().start()))
                .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
            out.push(Self::class(
                &node,
                item,
                resolved,
                inference,
                index,
                file,
                class_version,
            )?);
        }
        Ok(out)
    }

    /// Whether `node` is a type declaration nested directly inside another type's body.
    fn is_nested(node: &SyntaxNode) -> bool {
        node.parent()
            .is_some_and(|parent| ast::ClassBody::cast(parent).is_some())
    }

    /// Compile one type declaration.
    fn class(
        node: &SyntaxNode,
        item: ItemId,
        resolved: &Resolved,
        inference: &TypeInference,
        index: &ProjectIndex,
        file: FileId,
        class_version: u16,
    ) -> Result<CompiledClass> {
        // A non-`static` nested class holds its enclosing instance in a synthetic field, and every one
        // of its constructors takes that instance as an extra first parameter. The index computed its
        // descriptors from the declaration, so all of them would be one parameter short — which is a
        // `NoSuchMethodError` at the first `new`, not a missing convenience.
        // A nested interface, `@interface`, and `enum` are implicitly `static` and hold no enclosing
        // instance, so only a nested *class* can be an inner one. One that is holds its enclosing
        // instance in a synthetic field, and every constructor takes it as an extra first parameter.
        let encloses = (Self::is_nested(node)
            && !Self::has_modifier(node, jals_syntax::SyntaxKind::STATIC_KW)
            && matches!(index.item(item).kind, DefKind::Class))
        .then(|| Self::enclosing_name(node, index, file))
        .transpose()?;
        let internal_name = Descriptor::internal_name_of(item, index);
        // An `@interface` *is* an interface: its members are implicitly `public abstract`, it has no
        // constructor, and `ACC_INTERFACE` is set. `ACC_ANNOTATION` is the only thing on top.
        let is_annotation = index.item(item).kind == DefKind::AnnotationType;
        let is_interface = is_annotation || index.item(item).kind == DefKind::Interface;
        let is_enum = index.item(item).kind == DefKind::Enum;
        let is_record = index.item(item).kind == DefKind::Record;
        let context = Context {
            index,
            inference,
            resolved,
            file,
            this_class: internal_name.clone(),
            in_interface: is_interface,
            in_enum: is_enum,
            encloses: encloses.clone(),
            inner: Self::inner_classes_of(node, index, file),
            captures: Self::captures_of(node, resolved, index, file),
            this_item: item,
            lambdas: alloc::collections::BTreeMap::new(),
        };

        let mut pool = ConstantPool::new();
        let this_class = pool.class_index(&internal_name).ok_or(AsmError::PoolFull)?;
        // Only a project-internal supertype can be named here; anything else is `Object`, which is
        // also the right answer for a class with no `extends` clause at all.
        let super_item = index
            .item(item)
            .supertypes
            .iter()
            .map(|supertype| supertype.id)
            .find(|&id| index.item(id).kind != DefKind::Interface);
        // An `enum`'s supertype is `java.lang.Enum` and the source never writes it, so there is no
        // `extends` clause for the index to have recorded.
        let super_name = if is_enum {
            ENUM.to_owned()
        } else if is_record {
            RECORD.to_owned()
        } else {
            super_item.map_or_else(
                || "java/lang/Object".to_owned(),
                |id| Descriptor::internal_name_of(id, index),
            )
        };
        let super_class = pool.class_index(&super_name).ok_or(AsmError::PoolFull)?;
        // Every *interface* supertype, in the order the source listed them. Dropping them produced a
        // class the JVM loads and then refuses to dispatch through: an `invokeinterface` on a type whose
        // `interfaces` never mentioned it is `IncompatibleClassChangeError` at the first call.
        let mut interfaces = Vec::new();
        let mut interface_names = Vec::new();
        for supertype in &index.item(item).supertypes {
            if index.item(supertype.id).kind != DefKind::Interface {
                continue;
            }
            let name = Descriptor::internal_name_of(supertype.id, index);
            interfaces.push(pool.class_index(&name).ok_or(AsmError::PoolFull)?);
            interface_names.push(name);
        }
        // Every annotation type extends `java.lang.annotation.Annotation`, and the source never writes
        // it. Without it `Class.isAnnotation` is false and no reflective reader recognises the type.
        if is_annotation {
            let name = pool
                .class_index("java/lang/annotation/Annotation")
                .ok_or(AsmError::PoolFull)?;
            interfaces.push(name);
        }

        // An `enum`'s members live under an `EnumBody`, after the constants and the `;`.
        let members: Vec<SyntaxNode> = if is_enum {
            node.children()
                .find_map(ast::EnumBody::cast)
                .map(|body| {
                    body.members()
                        .map(|member| member.syntax().clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            node.children()
                .find(|child| ast::ClassBody::cast(child.clone()).is_some())
                .map(|body| body.children().collect())
                .unwrap_or_default()
        };
        let constants: Vec<ast::EnumConstant> = node
            .children()
            .find_map(ast::EnumBody::cast)
            .map(|body| body.constants().collect())
            .unwrap_or_default();

        if is_enum {
            Self::enum_shape(&constants)?;
        }

        let mut fields = Vec::new();
        let mut methods = Vec::new();
        // Worked out before any body is lowered, because a body cannot add a method to the class it is in.
        let (context, lambda_methods, bootstraps) =
            Self::synthesise_lambdas(context, &members, &mut pool)?;
        methods.extend(lambda_methods);

        let mut saw_constructor = false;

        for member in &members {
            // A nested type is its own class file, compiled by `file` rather than here. An `enum`, a
            // `record`, and an `@interface` are not compiled at all yet, and dropping one silently
            // would produce a class that loads and then throws `NoClassDefFoundError` at the first use
            // — the exact failure a compiler that reports nothing has to avoid.
            match member.kind() {
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | ANNOTATION_TYPE_DECL | RECORD_DECL => {
                    continue;
                }
                _ => {}
            }
            // A non-`static` nested class holds a reference to its enclosing instance, which means a
            // synthetic field *and* an extra parameter on every constructor — so the descriptors the
            // index computed from the declaration would all be one parameter short.
            match member.kind() {
                FIELD_DECL => Self::field(member, &context, &mut pool, &mut fields)?,
                METHOD_DECL => methods.push(Self::method(member, &context, &mut pool)?),
                // A record's compact constructor is the canonical one, emitted by `record_members` with
                // the components as its parameters. Emitting it here would give it `<init>()V` instead.
                CONSTRUCTOR_DECL if is_record && Self::is_compact_constructor(member) => {
                    saw_constructor = true;
                }
                CONSTRUCTOR_DECL => {
                    saw_constructor = true;
                    methods.push(Self::constructor(
                        member,
                        &context,
                        &mut pool,
                        &super_name,
                        super_item,
                        &members,
                    )?);
                }
                _ => {}
            }
        }
        // A class with no declared constructor gets the default one, which is the only place a
        // field initialiser could run for it.
        // An `enum` gets the synthesised `(String, int)` constructor instead, which is the only one
        // that can reach `Enum`'s — there is no no-argument `super()` for a default one to call.
        if !saw_constructor && !is_interface && !is_enum && !is_record {
            // An anonymous class's `new` may carry arguments, and they go to the *superclass*
            // constructor: the body declares none and has nowhere to write `super(…)`. Its own
            // constructor therefore takes that one's parameters and forwards them. Both sides read the
            // selection from the same span, so neither can pick a different constructor than the other.
            let forwarded = (node.kind() == jals_syntax::SyntaxKind::NEW_EXPR)
                .then(|| inference.call_target_of(Context::span(node)))
                .flatten()
                .map(|member| Descriptor::method_descriptor(member, index, true))
                .transpose()?;
            methods.push(Self::default_constructor(
                &context,
                &mut pool,
                &super_name,
                super_item,
                &members,
                Self::access_level(node),
                forwarded.as_ref(),
            )?);
        }
        // A record's components are declared once, in its header, and every one of them stands for a
        // field, an accessor, and a constructor parameter — none of which the body writes.
        let record_attribute = if is_record {
            Some(Self::record_members(
                node,
                &context,
                &mut pool,
                &members,
                &super_name,
                &mut fields,
                &mut methods,
            )?)
        } else {
            None
        };
        // An `enum`'s constants, its `$VALUES`, its constructor, and its two static methods are all
        // synthesised: none of them is written in the source, and every one of them is required.
        if is_enum {
            Self::enum_members(
                &constants,
                &context,
                &mut pool,
                &internal_name,
                &members,
                &mut fields,
                &mut methods,
            )?;
        }
        // An `assert` is guarded by a synthetic field the JVM's `-ea` flag decides, so a class
        // containing one gains that field and the `<clinit>` code that reads the flag.
        let asserts = node
            .descendants()
            .any(|child| child.kind() == jals_syntax::SyntaxKind::ASSERT_STMT);
        if asserts {
            // An interface field must be `public static final` (JVMS §4.5) — no exception for a
            // synthetic one. Emitting it package-private made an `assert` inside a `default` method a
            // `ClassFormatError: Illegal field modifiers` at load time.
            let visibility = if is_interface {
                FieldAccessFlags::PUBLIC
            } else {
                0
            };
            fields.push(FieldInfo {
                access_flags: FieldAccessFlags(
                    visibility
                        | FieldAccessFlags::STATIC
                        | FieldAccessFlags::FINAL
                        | FieldAccessFlags::SYNTHETIC,
                ),
                name_index: pool
                    .utf8_index(stmt::ASSERTIONS_DISABLED)
                    .ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index("Z").ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
            });
        }
        if let Some(enclosing) = &encloses {
            fields.push(Self::enclosing_field(enclosing, &mut pool)?);
        }
        // One `final synthetic` field per captured local, which is how the class outlives the frame the
        // local lived in.
        for &captured in context.captures.get(&item).into_iter().flatten() {
            let name = Self::capture_field(captured, &context);
            let descriptor = Self::capture_descriptor(captured, &context)?;
            fields.push(FieldInfo {
                access_flags: FieldAccessFlags(
                    FieldAccessFlags::FINAL | FieldAccessFlags::SYNTHETIC,
                ),
                name_index: pool.utf8_index(&name).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
            });
        }
        // A `static` field's initialiser and a `static { … }` block both run in `<clinit>`, once,
        // when the class is first used. Nothing else runs them — so dropping them produced a class
        // whose `static int n = 5;` read back as 0, which is a silent miscompile rather than a
        // missing feature.
        if let Some(class_init) = Self::class_initializer(
            &context,
            &mut pool,
            &members,
            asserts,
            if is_enum { constants.as_slice() } else { &[] },
            &internal_name,
        )? {
            methods.push(class_init);
        }

        // A generic supertype's method is *erased* in its own class file, so an override with a more
        // specific parameter type does not override it at all as far as the JVM is concerned: the two
        // descriptors differ. A bridge carries the erased signature and delegates.
        Self::bridges(item, &context, &mut pool, &members, &mut methods)?;

        let mut nesting = Self::inner_classes(node, &context, &mut pool)?;
        // A generic declaration's type parameters survive erasure only in this attribute. Nothing at run
        // time reads it — the JVM links on descriptors — but every reflective reader does, and a class
        // whose `Signature` is missing reports `Box` where the source wrote `Box<T>`.
        if let Some(signature) =
            Self::class_signature(node, &context, &super_name, &interface_names)?
        {
            let name_index = pool.utf8_index("Signature").ok_or(AsmError::PoolFull)?;
            let signature_index = pool.utf8_index(&signature).ok_or(AsmError::PoolFull)?;
            nesting.push(jals_classfile::Attribute {
                name_index,
                body: jals_classfile::AttributeBody::Signature { signature_index },
            });
        }
        if !bootstraps.is_empty() {
            // The attribute every `invokedynamic` in the class indexes into. Without it a call site names an
            // entry that is not there, which is a `ClassFormatError` before anything runs.
            let name_index = pool
                .utf8_index("BootstrapMethods")
                .ok_or(AsmError::PoolFull)?;
            nesting.push(jals_classfile::Attribute {
                name_index,
                body: jals_classfile::AttributeBody::BootstrapMethods(bootstraps),
            });
        }
        if let Some(components) = record_attribute {
            // The `Record` attribute is what makes `Class.isRecord` true and what every reflective
            // reader (and pattern matching) enumerates the components through. Without it the class is
            // an ordinary final class with some accessors.
            let name_index = pool.utf8_index("Record").ok_or(AsmError::PoolFull)?;
            nesting.push(jals_classfile::Attribute {
                name_index,
                body: jals_classfile::AttributeBody::Record(components),
            });
        }

        let mut class = ClassFile::new(class_version, 0, pool);
        let mut flags = Self::class_flags(node, is_interface, is_annotation);
        if is_enum {
            // `ACC_ENUM` is what makes `Enum.valueOf` and a `switch` over the type work at run time,
            // and `ACC_FINAL` is what an enum with no constant bodies is.
            flags |= ClassAccessFlags::ENUM | ClassAccessFlags::FINAL;
        }
        if is_record {
            // Every record is implicitly final (JLS §8.10), and the source never writes it.
            flags |= ClassAccessFlags::FINAL;
        }
        class.access_flags = ClassAccessFlags(flags);
        class.this_class = this_class;
        class.super_class = super_class;
        class.interfaces = interfaces;
        class.fields = fields;
        class.methods = methods;
        class.attributes = nesting;
        Ok(CompiledClass {
            internal_name,
            bytes: class.write(),
        })
    }

    /// The `InnerClasses` attribute: this type if it is nested, plus every type nested directly in it.
    ///
    /// Not optional decoration. A nested type's `private` or `static` cannot live in its own
    /// `access_flags` — the JVM has nowhere to put either — so this is the only record of what the
    /// source declared, and reflection reads it back from here. `getSimpleName` also comes from here;
    /// without it a nested class reports `Outer$Inner`.
    fn inner_classes(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
    ) -> Result<Vec<jals_classfile::Attribute>> {
        let mut nested: Vec<&SyntaxNode> = Vec::new();
        let own_body: Vec<SyntaxNode> = node
            .children()
            .find(|child| ast::ClassBody::cast(child.clone()).is_some())
            .map(|body| body.children().collect())
            .unwrap_or_default();
        let inner: Vec<&SyntaxNode> = own_body
            .iter()
            .filter(|child| {
                matches!(
                    child.kind(),
                    CLASS_DECL | INTERFACE_DECL | ENUM_DECL | ANNOTATION_TYPE_DECL | RECORD_DECL
                )
            })
            .collect();
        nested.extend(inner);
        if Self::is_nested(node) {
            nested.push(node);
        }
        if nested.is_empty() {
            return Ok(Vec::new());
        }

        let mut entries = Vec::with_capacity(nested.len());
        for declaration in nested {
            let name = declaration
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
                .ok_or(LowerError::Unsupported("a type declaration with no name"))?;
            let item = context
                .index
                .item_by_decl(context.file, usize::from(name.text_range().start()))
                .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
            let enclosing = declaration
                .ancestors()
                .skip(1)
                .find(|ancestor| {
                    matches!(
                        ancestor.kind(),
                        CLASS_DECL
                            | INTERFACE_DECL
                            | ENUM_DECL
                            | ANNOTATION_TYPE_DECL
                            | RECORD_DECL
                    )
                })
                .and_then(|outer| {
                    let token = outer
                        .children_with_tokens()
                        .filter_map(jals_syntax::SyntaxElement::into_token)
                        .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)?;
                    context
                        .index
                        .item_by_decl(context.file, usize::from(token.text_range().start()))
                })
                .ok_or(LowerError::Unsupported(
                    "a nested type with no enclosing type",
                ))?;

            let inner_index = pool
                .class_index(&Descriptor::internal_name_of(item, context.index))
                .ok_or(AsmError::PoolFull)?;
            let outer_index = pool
                .class_index(&Descriptor::internal_name_of(enclosing, context.index))
                .ok_or(AsmError::PoolFull)?;
            let name_index = pool.utf8_index(name.text()).ok_or(AsmError::PoolFull)?;
            // The flags the *source* wrote, which is where a nested type's `private` and `static` go.
            let kind = context.index.item(item).kind;
            let is_annotation = kind == DefKind::AnnotationType;
            let is_interface = is_annotation || kind == DefKind::Interface;
            let mut flags = Self::class_flags(declaration, is_interface, is_annotation)
                & !ClassAccessFlags::SUPER;
            flags |= Self::access_level(declaration);
            if Self::has_modifier(declaration, jals_syntax::SyntaxKind::STATIC_KW) {
                // `ClassAccessFlags` has no `STATIC`, because a *class* file cannot be static — only
                // an `InnerClasses` entry records it (JVMS §4.7.6, `ACC_STATIC` = 0x0008).
                flags |= MethodAccessFlags::STATIC;
            }
            entries.push(jals_classfile::InnerClassEntry::new(
                inner_index,
                outer_index,
                name_index,
                flags,
            ));
        }
        let name_index = pool.utf8_index("InnerClasses").ok_or(AsmError::PoolFull)?;
        Ok(alloc::vec![jals_classfile::Attribute {
            name_index,
            body: jals_classfile::AttributeBody::InnerClasses(entries),
        }])
    }

    /// The access-level bit a declaration carries, or `0` for package-private.
    ///
    /// The four Java access levels are one *choice*; `static` / `final` / `abstract` are
    /// independent bits. Folding the two together is what used to clear `ACC_PUBLIC` from every
    /// `public static` method — a `public` helper that a class in another package then could not
    /// call, with `IllegalAccessError` as the only report. Package-private is the absence of a bit
    /// rather than a bit of its own (JVMS §4.6), so there is nothing to set for it.
    ///
    /// The three constants coincide across `ClassAccessFlags` / `FieldAccessFlags` /
    /// `MethodAccessFlags` (JVMS tables 4.1-B, 4.5-A, 4.6-A), so one function answers for all
    /// three.
    fn access_level(node: &SyntaxNode) -> u16 {
        use jals_syntax::SyntaxKind::{PRIVATE_KW, PROTECTED_KW, PUBLIC_KW};
        if Self::has_modifier(node, PRIVATE_KW) {
            MethodAccessFlags::PRIVATE
        } else if Self::has_modifier(node, PROTECTED_KW) {
            MethodAccessFlags::PROTECTED
        } else if Self::has_modifier(node, PUBLIC_KW) {
            MethodAccessFlags::PUBLIC
        } else {
            0
        }
    }

    fn class_flags(node: &SyntaxNode, is_interface: bool, is_annotation: bool) -> u16 {
        // Only `public` is expressible on a top-level type. `private` / `protected` are nested-type
        // modifiers, and a nested type is reported rather than emitted.
        let mut flags = Self::access_level(node) & ClassAccessFlags::PUBLIC;
        if is_interface {
            // An interface is implicitly abstract and never has the `super`-call semantics bit.
            flags |= ClassAccessFlags::INTERFACE | ClassAccessFlags::ABSTRACT;
            if is_annotation {
                flags |= ClassAccessFlags::ANNOTATION;
            }
        } else {
            // `ACC_SUPER` selects the modern `invokespecial` semantics; every class emitted today
            // wants it, and the JVM ignores it from version 52 on.
            flags |= ClassAccessFlags::SUPER;
        }
        if Self::has_modifier(node, jals_syntax::SyntaxKind::FINAL_KW) {
            flags |= ClassAccessFlags::FINAL;
        }
        if !is_interface && Self::has_modifier(node, jals_syntax::SyntaxKind::ABSTRACT_KW) {
            flags |= ClassAccessFlags::ABSTRACT;
        }
        flags
    }

    /// The locals a class declared inside a block captures, in source order and without repeats.
    ///
    /// A capture is what it sounds like: a name inside the class that resolves to a definition *outside*
    /// it. Each becomes a `final synthetic` field and a trailing constructor parameter, which is how a
    /// class outlives the frame the local lived in.
    fn captured_by(node: &SyntaxNode, resolved: &Resolved) -> alloc::vec::Vec<jals_hir::DefId> {
        let mut out = alloc::vec::Vec::new();
        let inside_block = node
            .ancestors()
            .skip(1)
            .any(|ancestor| ancestor.kind() == jals_syntax::SyntaxKind::BLOCK);
        if !inside_block {
            return out;
        }
        let range = node.text_range();
        for token in node
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
        {
            let Some(id) = resolved
                .reference_at(usize::from(token.text_range().start()))
                .and_then(|reference| reference.resolution.def_id())
            else {
                continue;
            };
            let def = resolved.def(id);
            // Only a *local* is captured: a field of the enclosing class is reached through its instance,
            // and a type name is not a value at all.
            if !matches!(def.kind, DefKind::Local | DefKind::Param) {
                continue;
            }
            let Ok(start) = u32::try_from(def.name_range.start) else {
                continue;
            };
            if !range.contains(start.into()) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Every local class in the file `node` belongs to, mapped to the locals it captures.
    fn captures_of(
        node: &SyntaxNode,
        resolved: &Resolved,
        index: &ProjectIndex,
        file: FileId,
    ) -> alloc::collections::BTreeMap<ItemId, alloc::vec::Vec<jals_hir::DefId>> {
        let mut out = alloc::collections::BTreeMap::new();
        let Some(root) = node.ancestors().last() else {
            return out;
        };
        for declaration in root.descendants() {
            // An anonymous class captures like a local one, and its declaration is the `new` — but what
            // is scanned is its *body*. Scanning the whole `new` would count a local the arguments name
            // as captured, and an argument is evaluated at the creation site rather than read from a
            // field: `new Base(n) { }` with a body that never says `n` captures nothing.
            let (scanned, at) = match declaration.kind() {
                CLASS_DECL => (
                    declaration.clone(),
                    declaration
                        .children_with_tokens()
                        .filter_map(jals_syntax::SyntaxElement::into_token)
                        .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
                        .map(|token| usize::from(token.text_range().start())),
                ),
                jals_syntax::SyntaxKind::NEW_EXPR => {
                    let Some(body) = declaration
                        .children()
                        .find(|child| child.kind() == CLASS_BODY)
                    else {
                        continue;
                    };
                    (body, Some(usize::from(declaration.text_range().start())))
                }
                _ => continue,
            };
            let captured = Self::captured_by(&scanned, resolved);
            if captured.is_empty() {
                continue;
            }
            let Some(at) = at else {
                continue;
            };
            if let Some(item) = index.item_by_decl(file, at) {
                out.insert(item, captured);
            }
        }
        out
    }

    /// The synthetic field name a captured local gets, as javac names it.
    fn capture_field(id: jals_hir::DefId, context: &Context<'_>) -> String {
        alloc::format!("val${}", context.resolved.def(id).name)
    }

    /// The descriptor of a captured local's type.
    fn capture_descriptor(id: jals_hir::DefId, context: &Context<'_>) -> Result<String> {
        Ok(
            Descriptor::descriptor_of(context.inference.type_of_def(id), context.index)?
                .to_string(),
        )
    }

    /// Find every lambda in `members`, synthesise the method that holds each body, and build the
    /// `BootstrapMethods` entry that links it.
    ///
    /// Only a *non-capturing* lambda with an expression body is emitted. A capturing one needs its captures
    /// as leading parameters of both the synthetic method and the call site, and a block body needs the
    /// statement lowering; each is its own step, and reporting beats emitting a call site whose arguments do
    /// not match its handle.
    fn synthesise_lambdas<'a>(
        mut context: Context<'a>,
        members: &[SyntaxNode],
        pool: &mut ConstantPool,
    ) -> Result<(
        Context<'a>,
        Vec<MethodInfo>,
        Vec<jals_classfile::BootstrapMethod>,
    )> {
        const METAFACTORY: &str = "java/lang/invoke/LambdaMetafactory";
        const METAFACTORY_DESCRIPTOR: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";
        let mut out = Vec::new();
        let mut bootstraps = Vec::new();
        let lambdas: Vec<SyntaxNode> = members
            .iter()
            .flat_map(SyntaxNode::descendants)
            .filter(|node| {
                matches!(
                    node.kind(),
                    jals_syntax::SyntaxKind::LAMBDA_EXPR | jals_syntax::SyntaxKind::METHOD_REF_EXPR
                )
            })
            .collect();
        for (ordinal, lambda) in lambdas.iter().enumerate() {
            // A method reference needs no synthetic method at all: the handle points straight at the method
            // the source named, which is the whole difference between the two forms.
            if lambda.kind() == jals_syntax::SyntaxKind::METHOD_REF_EXPR {
                let (call, entry) = Self::method_reference(lambda, &context, pool)?;
                let index = u16::try_from(bootstraps.len()).map_err(|_| AsmError::PoolFull)?;
                bootstraps.push(entry);
                let span = Context::span(lambda);
                context.lambdas.insert(
                    (span.start, span.end),
                    Lambda {
                        bootstrap: index,
                        ..call
                    },
                );
                continue;
            }
            let decl = ast::LambdaExpr::cast(lambda.clone())
                .ok_or(LowerError::Unsupported("a malformed lambda"))?;

            // The interface the context asked for, and the one method it declares.
            let item = expr::Expr::type_of(lambda, &context)?
                .project_id()
                .ok_or(LowerError::Unsupported("a lambda with no target type"))?;
            let member = Self::functional_member(item, &context).ok_or(LowerError::Unsupported(
                "a lambda target with no single method",
            ))?;
            let name = context.index.member(member).name.clone();
            let descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
                member,
                context.index,
                false,
            )?);
            let interface = Descriptor::internal_name_of(item, context.index);
            let returns = context.index.resolved_member_ty(member);
            // Each captured local is a *leading* parameter of the synthetic method and an argument of the
            // call site — leading, because the metafactory prepends the captured values to the interface
            // method's own arguments when it invokes the handle.
            // A capturing lambda is *nearly* here: the captures are collected, the leading parameters and
            // the call-site arguments line up, and the body still reads `bump` as neither a local nor a
            // field. Reported until that is chased down, rather than emitted as a handle whose parameter
            // nothing fills.
            let captured = Self::captured_by(lambda, context.resolved);

            // A capturing lambda: the descriptor, the leading slots, and the call-site arguments now all
            // line up, and the synthetic method still fails the assembler's frame check — reported until
            // that is chased down, rather than emitted as code the verifier would reject.
            let mut prefix = String::new();
            for &id in &captured {
                prefix.push_str(&Self::capture_descriptor(id, &context)?);
            }
            let synthetic_descriptor =
                alloc::format!("({prefix}{}", descriptor.trim_start_matches('('));

            // The synthetic method takes the interface's descriptor with the captures prepended, which is
            // also what seeds its initial locals. The assembler borrows the pool for as long as it lives,
            // so its code comes out first and every entry the method *info* needs is interned after.
            let synthetic = alloc::format!("lambda${ordinal}");
            let code = {
                let mut asm = Assembler::new(pool, Receiver::Static, &synthetic_descriptor)?;
                let mut slots = Slots::new(&context, None, true);
                // The captures come first, in the order the call site pushes them: the metafactory prepends
                // the captured values to the interface method's own arguments when it invokes the handle.
                for &id in &captured {
                    let width = Slots::ty_width(context.inference.type_of_def(id));
                    slots.declare(id, width);
                }
                for param in decl.params().into_iter().flat_map(|list| list.params()) {
                    let id = context
                        .def_at(param.syntax())
                        .ok_or(LowerError::Unsupported(
                            "a lambda parameter with no binding",
                        ))?;
                    let width = Slots::ty_width(context.inference.type_of_def(id));
                    slots.declare(id, width);
                }
                let mut emit = Emit::new(&mut asm, slots, returns.clone(), false);
                match (decl.expr_body(), decl.block_body()) {
                    // An expression body *is* the returned value, or is evaluated for its effect when the
                    // interface method returns nothing.
                    (Some(value), _) => {
                        if matches!(returns, jals_hir::Ty::Void) {
                            stmt::Stmt::discarded(&value, &context, &mut emit)?;
                            asm.return_(None)?;
                        } else {
                            expr::Expr::lower_as(&value, &returns, &context, &mut emit)?;
                            let top = asm
                                .stack_top()
                                .ok_or(LowerError::Unsupported("a lambda body with no value"))?;
                            asm.return_(Some(&top))?;
                        }
                    }
                    // A block body returns for itself, except that a `void` one may run off its end.
                    (None, Some(block)) => {
                        stmt::Stmt::block(&block, &context, &mut emit)?;
                        if matches!(returns, jals_hir::Ty::Void) && asm.reachable() {
                            asm.return_(None)?;
                        }
                    }
                    (None, None) => return Err(LowerError::Unsupported("a lambda with no body")),
                }
                asm.finish()?
            };
            out.push(MethodInfo {
                // private | static | synthetic
                access_flags: MethodAccessFlags(0x0002 | 0x0008 | 0x1000),
                name_index: pool.utf8_index(&synthetic).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool
                    .utf8_index(&synthetic_descriptor)
                    .ok_or(AsmError::PoolFull)?,
                attributes: alloc::vec![code],
            });

            // `metafactory` is handed the interface's shape, a handle to the body, and the shape again — the
            // two `MethodType`s differ only where generics erase, which this does not model.
            let handle = pool
                .method_handle_index(
                    6,
                    &context.this_class,
                    &synthetic,
                    &synthetic_descriptor,
                    false,
                )
                .ok_or(AsmError::PoolFull)?;
            let shape = pool
                .method_type_index(&descriptor)
                .ok_or(AsmError::PoolFull)?;
            let bootstrap = pool
                .method_handle_index(6, METAFACTORY, "metafactory", METAFACTORY_DESCRIPTOR, false)
                .ok_or(AsmError::PoolFull)?;
            bootstraps.push(jals_classfile::BootstrapMethod {
                bootstrap_method_ref: bootstrap,
                bootstrap_arguments: alloc::vec![shape, handle, shape],
            });
            let index = u16::try_from(bootstraps.len() - 1).map_err(|_| AsmError::PoolFull)?;
            let span = Context::span(lambda);
            context.lambdas.insert(
                (span.start, span.end),
                Lambda {
                    interface_method: name,
                    call_descriptor: alloc::format!("({prefix})L{interface};"),
                    bootstrap: index,
                    captured,
                },
            );
        }
        Ok((context, out, bootstraps))
    }

    /// A `Type::method` reference: the call site it needs, and the `BootstrapMethods` entry that links it.
    ///
    /// Only a reference to a `static` method of a named type. An instance one (`x::m`) captures the receiver,
    /// and a constructor one (`T::new`) needs `newInvokeSpecial` — each is its own kind of handle.
    fn method_reference(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
    ) -> Result<(Lambda, jals_classfile::BootstrapMethod)> {
        const METAFACTORY: &str = "java/lang/invoke/LambdaMetafactory";
        const METAFACTORY_DESCRIPTOR: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";
        // The interface the context asked for, and the one method it declares.
        let item =
            expr::Expr::type_of(node, context)?
                .project_id()
                .ok_or(LowerError::Unsupported(
                    "a method reference with no target type",
                ))?;
        let member = Self::functional_member(item, context)
            .ok_or(LowerError::Unsupported("a target with no single method"))?;
        let name = context.index.member(member).name.clone();
        let descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
            member,
            context.index,
            false,
        )?);
        let interface = Descriptor::internal_name_of(item, context.index);

        // A constructor reference names `new` rather than a method: the handle is `newInvokeSpecial` on the
        // type's own constructor, and there is nothing to capture.
        let constructs = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| token.kind() == jals_syntax::SyntaxKind::NEW_KW);

        // The class the method belongs to. `Uses::twice` parses its qualifier as an *expression* — a name
        // reference is what a type name looks like before anything resolves it — so both spellings are read.
        let qualifier = node.children().find_map(ast::Expr::cast);
        let named_type = if let Some(owner_ty) = node.children().find_map(ast::Type::cast) {
            context.ty_of_type(&owner_ty)?.project_id()
        } else {
            qualifier
                .as_ref()
                .and_then(|q| context.ty_of_name(q.syntax()).ok())
                .and_then(|ty| ty.project_id())
        };
        // Not a type: the qualifier is a *value*, so the reference is bound to it and the receiver is what
        // the call site captures. Only a local is read, because a capture is loaded from a slot.
        let (owner_item, receiver) = if let Some(item) = named_type {
            (item, None)
        } else {
            {
                let expr = qualifier.as_ref().ok_or(LowerError::Unsupported(
                    "a method reference with no qualifier",
                ))?;
                let id = context
                    .def_at(expr.syntax())
                    .ok_or(LowerError::Unsupported(
                        "a method reference whose qualifier is no local",
                    ))?;
                let item = context.inference.type_of_def(id).project_id().ok_or(
                    LowerError::Unsupported("a method reference on a value of an unindexed type"),
                )?;
                (item, Some(id))
            }
        };
        if constructs {
            return Self::constructor_reference(owner_item, member, item, context, pool);
        }
        let arity = context.index.member(member).params.len();
        // Set when the referenced method turns out to be an instance one named through its *type*, whose
        // receiver the interface supplies as its first argument.
        let unbound = core::cell::Cell::new(false);
        // The method's own name is a direct token of the reference: everything before the `::` is a node.
        let referenced = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            .last()
            .ok_or(LowerError::Unsupported("a method reference with no name"))?;
        let target = context
            .index
            .own_members(owner_item)
            .iter()
            .copied()
            .find(|&id| {
                let info = context.index.member(id);
                info.kind == DefKind::Method
                    && info.name == referenced.text()
                    && info.modifiers.is_static == receiver.is_none()
                    // A bound reference passes the receiver separately, so the interface method's own
                    // arity is what the referenced method takes.
                    && info.params.len() == arity
            })
            // Not found as that shape: a reference qualified by a *type* may still name an instance method,
            // and then the interface's first argument is the receiver — `Type::method` with one fewer
            // parameter than the interface declares. That is the *unbound* form.
            .or_else(|| {
                (receiver.is_none() && arity > 0)
                    .then(|| {
                        context
                            .index
                            .own_members(owner_item)
                            .iter()
                            .copied()
                            .find(|&id| {
                                let info = context.index.member(id);
                                info.kind == DefKind::Method
                                    && info.name == referenced.text()
                                    && !info.modifiers.is_static
                                    && info.params.len() == arity - 1
                            })
                    })
                    .flatten()
                    .inspect(|_| unbound.set(true))
            })
            .ok_or(LowerError::Unsupported(
                "a method reference to a method this cannot find",
            ))?;
        let owner = Descriptor::internal_name_of(owner_item, context.index);
        let target_descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
            target,
            context.index,
            false,
        )?);

        // 6 is `invokeStatic` and 5 is `invokeVirtual` (JVMS Table 5.4.3.5-A): both a bound reference and an
        // unbound one call the method *on* a receiver — the difference is only where that receiver comes
        // from, and the handle cannot tell.
        let kind = if receiver.is_some() || unbound.get() {
            5
        } else {
            6
        };
        let handle = pool
            .method_handle_index(kind, &owner, referenced.text(), &target_descriptor, false)
            .ok_or(AsmError::PoolFull)?;
        let shape = pool
            .method_type_index(&descriptor)
            .ok_or(AsmError::PoolFull)?;
        let bootstrap = pool
            .method_handle_index(6, METAFACTORY, "metafactory", METAFACTORY_DESCRIPTOR, false)
            .ok_or(AsmError::PoolFull)?;
        // A bound reference's call site takes the receiver, which is the one value it captures.
        let (prefix, captured) = receiver.map_or_else(
            || (String::new(), alloc::vec::Vec::new()),
            |id| (alloc::format!("L{owner};"), alloc::vec![id]),
        );
        Ok((
            Lambda {
                interface_method: name,
                call_descriptor: alloc::format!("({prefix})L{interface};"),
                bootstrap: 0,
                captured,
            },
            jals_classfile::BootstrapMethod {
                bootstrap_method_ref: bootstrap,
                bootstrap_arguments: alloc::vec![shape, handle, shape],
            },
        ))
    }

    /// `T::new`: the handle is `newInvokeSpecial` on the type's own constructor, and nothing is captured.
    fn constructor_reference(
        owner_item: ItemId,
        member: jals_hir::MemberId,
        target: ItemId,
        context: &Context<'_>,
        pool: &mut ConstantPool,
    ) -> Result<(Lambda, jals_classfile::BootstrapMethod)> {
        const METAFACTORY: &str = "java/lang/invoke/LambdaMetafactory";
        const METAFACTORY_DESCRIPTOR: &str = "(Ljava/lang/invoke/MethodHandles$Lookup;Ljava/lang/String;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodType;Ljava/lang/invoke/MethodHandle;Ljava/lang/invoke/MethodType;)Ljava/lang/invoke/CallSite;";
        let owner = Descriptor::internal_name_of(owner_item, context.index);
        let interface = Descriptor::internal_name_of(target, context.index);
        let name = context.index.member(member).name.clone();
        let descriptor = MethodDescriptor::to_string(&Descriptor::method_descriptor(
            member,
            context.index,
            false,
        )?);
        // A class with no declared constructor has the implicit no-argument one, whose descriptor is fixed.
        let arity = context.index.member(member).params.len();
        let constructor = context
            .index
            .own_members(owner_item)
            .iter()
            .copied()
            .find(|&id| {
                let info = context.index.member(id);
                info.kind == DefKind::Constructor && info.params.len() == arity
            });
        let target_descriptor = match constructor {
            Some(id) => MethodDescriptor::to_string(&Descriptor::method_descriptor(
                id,
                context.index,
                true,
            )?),
            None if arity == 0 => "()V".to_owned(),
            None => {
                return Err(LowerError::Unsupported(
                    "a constructor reference with no matching constructor",
                ));
            }
        };
        // 8 is `newInvokeSpecial`: the handle allocates as well as initialises, which is what makes the
        // factory the interface asks for.
        let handle = pool
            .method_handle_index(8, &owner, "<init>", &target_descriptor, false)
            .ok_or(AsmError::PoolFull)?;
        let shape = pool
            .method_type_index(&descriptor)
            .ok_or(AsmError::PoolFull)?;
        let bootstrap = pool
            .method_handle_index(6, METAFACTORY, "metafactory", METAFACTORY_DESCRIPTOR, false)
            .ok_or(AsmError::PoolFull)?;
        Ok((
            Lambda {
                interface_method: name,
                call_descriptor: alloc::format!("()L{interface};"),
                bootstrap: 0,
                captured: alloc::vec::Vec::new(),
            },
            jals_classfile::BootstrapMethod {
                bootstrap_method_ref: bootstrap,
                bootstrap_arguments: alloc::vec![shape, handle, shape],
            },
        ))
    }

    /// The one method a functional interface declares, or `None` when it declares none or several.
    fn functional_member(item: ItemId, context: &Context<'_>) -> Option<jals_hir::MemberId> {
        let mut methods = context
            .index
            .own_members(item)
            .iter()
            .copied()
            .filter(|&id| context.index.member(id).kind == DefKind::Method);
        let only = methods.next()?;
        methods.next().is_none().then_some(only)
    }

    /// Every inner class declared in the file `node` belongs to, mapped to the internal name of the class
    /// it holds an instance of.
    ///
    /// Walked from the file root rather than passed in, because a `new Inner()` may sit in any class in
    /// the file and each is compiled on its own — the creation needs the *target's* shape, not its own.
    fn inner_classes_of(
        node: &SyntaxNode,
        index: &ProjectIndex,
        file: FileId,
    ) -> alloc::collections::BTreeMap<ItemId, String> {
        let mut out = alloc::collections::BTreeMap::new();
        let Some(root) = node.ancestors().last() else {
            return out;
        };
        for declaration in root.descendants().filter(|n| n.kind() == CLASS_DECL) {
            if !Self::is_nested(&declaration)
                || Self::has_modifier(&declaration, jals_syntax::SyntaxKind::STATIC_KW)
            {
                continue;
            }
            let Some(name) = declaration
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            else {
                continue;
            };
            let Some(item) = index.item_by_decl(file, usize::from(name.text_range().start()))
            else {
                continue;
            };
            if let Ok(enclosing) = Self::enclosing_name(&declaration, index, file) {
                out.insert(item, enclosing);
            }
        }
        out
    }

    /// One synthetic bridge per method whose erased descriptor differs from the inherited one it overrides.
    ///
    /// `class Box implements Holder<String> { public void put(String s) {} }` declares `put(String)`, and
    /// `Holder.put` erases to `put(Object)`. Without a bridge the class has no `put(Object)` at all, so a
    /// call through `Holder` finds nothing to dispatch to — an `AbstractMethodError` at run time, and the
    /// one thing erasure cannot be left to sort out by itself.
    fn bridges(
        item: ItemId,
        context: &Context<'_>,
        pool: &mut ConstantPool,
        members: &[SyntaxNode],
        methods: &mut Vec<MethodInfo>,
    ) -> Result<()> {
        for member in members {
            if member.kind() != METHOD_DECL {
                continue;
            }
            let Some(name) = ast::MethodDecl::cast(member.clone()).and_then(|decl| decl.name())
            else {
                continue;
            };
            let Some(token) = member
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            else {
                continue;
            };
            let own = context.member_at(&token)?;
            if context.index.member(own).modifiers.is_static {
                continue;
            }
            let own_text = MethodDescriptor::to_string(&Descriptor::method_descriptor(
                own,
                context.index,
                false,
            )?);
            // Every inherited method of the same name and arity: an override of a *generic* one erases
            // differently, and that difference is exactly what needs bridging.
            for &inherited in &context.index.members_of(item) {
                let info = context.index.member(inherited);
                if inherited == own
                    || info.owner == item
                    || info.kind != DefKind::Method
                    || info.name != name
                    || info.params.len() != context.index.member(own).params.len()
                {
                    continue;
                }
                // The declaring type's own parameters, so `Holder<T>.put(T)` erases to `put(Object)`
                // rather than failing on a name the index resolves to nothing.
                let vars: Vec<String> = context
                    .index
                    .item(info.owner)
                    .type_params
                    .iter()
                    .map(|param| param.name.clone())
                    .collect();
                let Ok(descriptor) =
                    Descriptor::method_descriptor_erasing(inherited, context.index, false, &vars)
                else {
                    continue;
                };
                let text = MethodDescriptor::to_string(&descriptor);
                if text == own_text {
                    continue;
                }
                methods.push(Self::bridge(
                    context,
                    pool,
                    &name,
                    &text,
                    &own_text,
                    &descriptor,
                )?);
                break;
            }
        }
        Ok(())
    }

    /// One bridge: take the erased arguments, cast each to what the override declared, and call it.
    fn bridge(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        name: &str,
        erased: &str,
        target: &str,
        descriptor: &MethodDescriptor,
    ) -> Result<MethodInfo> {
        let name_index = pool.utf8_index(name).ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(erased).ok_or(AsmError::PoolFull)?;
        let target_descriptor = MethodDescriptor::parse(target)
            .map_err(|_| LowerError::Unsupported("a bridge with an unreadable target"))?;
        let mut asm = Assembler::new(pool, Receiver::Instance(&context.this_class), erased)?;
        asm.load(0)?;
        let mut slot = 1u16;
        for (position, param) in descriptor.params.iter().enumerate() {
            asm.load(slot)?;
            // The override declared something narrower, so the erased argument is cast to it — which is
            // the `checkcast` javac emits and the reason a bridge can throw `ClassCastException`.
            if let Some(jals_classfile::FieldType::Object(narrower)) =
                target_descriptor.params.get(position)
                && matches!(param, jals_classfile::FieldType::Object(wider) if *wider != *narrower)
            {
                asm.check_cast(narrower)?;
            }
            slot += Slots::descriptor_width(&param.to_string());
        }
        asm.invoke_virtual(&context.this_class, name, target)?;
        match asm.stack_top() {
            Some(top) => asm.return_(Some(&top))?,
            None => asm.return_(None)?,
        }
        Ok(MethodInfo {
            // public | bridge | synthetic
            access_flags: MethodAccessFlags(0x0001 | 0x0040 | 0x1000),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// The internal name of the class a nested declaration sits inside.
    fn enclosing_name(node: &SyntaxNode, index: &ProjectIndex, file: FileId) -> Result<String> {
        let declaration =
            node.parent()
                .and_then(|body| body.parent())
                .ok_or(LowerError::Unsupported(
                    "an inner class with no enclosing type",
                ))?;
        let name = declaration
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            .ok_or(LowerError::Unsupported("an enclosing type with no name"))?;
        let enclosing = index
            .item_by_decl(file, usize::from(name.text_range().start()))
            .ok_or_else(|| LowerError::Unresolved(name.text().into()))?;
        Ok(Descriptor::internal_name_of(enclosing, index))
    }

    /// The synthetic field an inner class holds its enclosing instance in.
    ///
    /// `final` so nothing can rebind it and `synthetic` because the source never wrote it — which is also
    /// what keeps a reflective reader from listing it among the class's declared fields.
    fn enclosing_field(enclosing: &str, pool: &mut ConstantPool) -> Result<FieldInfo> {
        Ok(FieldInfo {
            access_flags: FieldAccessFlags(FieldAccessFlags::FINAL | FieldAccessFlags::SYNTHETIC),
            name_index: pool.utf8_index(OUTER).ok_or(AsmError::PoolFull)?,
            descriptor_index: pool
                .utf8_index(&alloc::format!("L{enclosing};"))
                .ok_or(AsmError::PoolFull)?,
            attributes: Vec::new(),
        })
    }

    /// Whether a declaration's `MODIFIERS` child carries `keyword`.
    fn has_modifier(node: &SyntaxNode, keyword: jals_syntax::SyntaxKind) -> bool {
        node.children()
            .find(|child| child.kind() == jals_syntax::SyntaxKind::MODIFIERS)
            .is_some_and(|modifiers| {
                modifiers
                    .children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|token| token.kind() == keyword)
            })
    }

    /// A method's or constructor's access flags.
    ///
    /// `in_interface` supplies the level JLS §9.4 leaves unwritten: an interface method with no
    /// explicit access level is `public`, and emitting it package-private would produce a class
    /// the verifier rejects.
    /// A method's or constructor's access flags.
    ///
    /// `strictfp` is deliberately not emitted. `ACC_STRICT` is a method flag only for major
    /// versions 46–60 (JVMS §4.6); from 61 on, strict floating point is the *only* semantics there
    /// is, so dropping the bit changes nothing about the program and setting it would set a bit the
    /// version reserves.
    fn method_flags(node: &SyntaxNode, in_interface: bool) -> u16 {
        use jals_syntax::SyntaxKind::{
            ABSTRACT_KW, FINAL_KW, NATIVE_KW, STATIC_KW, SYNCHRONIZED_KW,
        };
        let level = match Self::access_level(node) {
            0 if in_interface => MethodAccessFlags::PUBLIC,
            level => level,
        };
        let mut flags = level;
        for (keyword, bit) in [
            (STATIC_KW, MethodAccessFlags::STATIC),
            (FINAL_KW, MethodAccessFlags::FINAL),
            (ABSTRACT_KW, MethodAccessFlags::ABSTRACT),
            (SYNCHRONIZED_KW, MethodAccessFlags::SYNCHRONIZED),
            (NATIVE_KW, MethodAccessFlags::NATIVE),
        ] {
            if Self::has_modifier(node, keyword) {
                flags |= bit;
            }
        }
        flags
    }

    /// A field's access flags, over `FieldAccessFlags`' own table — `transient` and `volatile`
    /// occupy bits a method uses for `bridge` and `synchronized`, so the two cannot share a
    /// keyword list even though their access levels coincide.
    ///
    /// `in_interface` supplies JLS §9.3: an interface field is implicitly `public static final`.
    fn field_flags(node: &SyntaxNode, in_interface: bool) -> u16 {
        use jals_syntax::SyntaxKind::{FINAL_KW, STATIC_KW, TRANSIENT_KW, VOLATILE_KW};
        let mut flags = match Self::access_level(node) {
            0 if in_interface => FieldAccessFlags::PUBLIC,
            level => level,
        };
        if in_interface {
            flags |= FieldAccessFlags::STATIC | FieldAccessFlags::FINAL;
        }
        for (keyword, bit) in [
            (STATIC_KW, FieldAccessFlags::STATIC),
            (FINAL_KW, FieldAccessFlags::FINAL),
            (TRANSIENT_KW, FieldAccessFlags::TRANSIENT),
            (VOLATILE_KW, FieldAccessFlags::VOLATILE),
        ] {
            if Self::has_modifier(node, keyword) {
                flags |= bit;
            }
        }
        flags
    }

    /// Emit a field declaration's `field_info`s — one per declarator, since `int a, b;` is one
    /// declaration and two fields.
    fn field(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
        out: &mut Vec<FieldInfo>,
    ) -> Result<()> {
        let Some(decl) = ast::FieldDecl::cast(node.clone()) else {
            return Ok(());
        };
        let vars = Self::type_variables(node);
        let written = node.children().find_map(ast::Type::cast);
        // A field whose type mentions a type variable keeps it here: erasure writes the *bound* into the
        // descriptor, so `T value` and `Object value` are the same field without this.
        let signature = match &written {
            Some(ty) if Self::mentions_variable(ty, &vars) => {
                Some(Self::type_signature(ty, &vars, context)?)
            }
            _ => None,
        };
        for name in decl.names() {
            let member = context.member_at(&name)?;
            let descriptor = Descriptor::field_descriptor(member, context.index)?.to_string();
            let mut attributes = Vec::new();
            if let Some(signature) = &signature {
                let name_index = pool.utf8_index("Signature").ok_or(AsmError::PoolFull)?;
                let signature_index = pool.utf8_index(signature).ok_or(AsmError::PoolFull)?;
                attributes.push(jals_classfile::Attribute {
                    name_index,
                    body: jals_classfile::AttributeBody::Signature { signature_index },
                });
            }
            out.push(FieldInfo {
                access_flags: FieldAccessFlags(Self::field_flags(node, context.in_interface)),
                name_index: pool.utf8_index(name.text()).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes,
            });
        }
        Ok(())
    }

    fn method(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
    ) -> Result<MethodInfo> {
        let decl = ast::MethodDecl::cast(node.clone())
            .ok_or(LowerError::Unsupported("a malformed method declaration"))?;
        let name = decl.name().unwrap_or_default();
        let member = context.member_at(
            &jals_syntax::ast::AstNode::syntax(&decl)
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
                .ok_or_else(|| LowerError::Unresolved(name.clone()))?,
        )?;
        // The method's own type parameters are not the class's, so the index resolved each as an
        // external name it has never heard of. Naming them here is what lets the descriptor erase them.
        let own_vars: Vec<String> = node
            .children()
            .find_map(ast::TypeParams::cast)
            .map(|params| params.params().filter_map(|param| param.name()).collect())
            .unwrap_or_default();
        let descriptor =
            Descriptor::method_descriptor_erasing(member, context.index, false, &own_vars)?;
        let is_static = context.index.member(member).modifiers.is_static;

        let flags = Self::method_flags(node, context.in_interface);
        let text = MethodDescriptor::to_string(&descriptor);
        let name_index = pool.utf8_index(&name).ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&text).ok_or(AsmError::PoolFull)?;
        let mut attributes = match decl.body() {
            Some(body) => {
                let receiver = if is_static {
                    Receiver::Static
                } else {
                    Receiver::Instance(&context.this_class)
                };
                let mut asm = Assembler::new(pool, receiver, &text)?;
                let slots = Slots::new(context, decl.params().as_ref(), is_static);
                let returns = context.index.resolved_member_ty(member);
                let mut emit = Emit::new(&mut asm, slots, returns, !is_static);
                stmt::Stmt::block(&body, context, &mut emit)?;
                // A `void` body may simply run off its end; the JVM needs the instruction anyway.
                // One that already returned on every path does not — and asking the assembler for
                // an unreachable `return` would be an error, not a no-op.
                if matches!(descriptor.return_type, jals_classfile::ReturnType::Void)
                    && asm.reachable()
                {
                    asm.return_(None)?;
                }
                alloc::vec![asm.finish()?]
            }
            // An abstract or interface method has no `Code` attribute at all.
            None => Vec::new(),
        };
        // A method that declares type parameters, or whose signature mentions one in scope, keeps them
        // here: `T first(List<T> xs)` and `Object first(List xs)` are otherwise the same method.
        if let Some(signature) = Self::method_signature(node, context)? {
            let name_index = pool.utf8_index("Signature").ok_or(AsmError::PoolFull)?;
            let signature_index = pool.utf8_index(&signature).ok_or(AsmError::PoolFull)?;
            attributes.push(jals_classfile::Attribute {
                name_index,
                body: jals_classfile::AttributeBody::Signature { signature_index },
            });
        }
        // `int count() default 3` is an annotation element's default, and the *only* place it lives in
        // the class file is this attribute. Dropping it compiles `@Marker` — an omitted element with a
        // default — into a use no reader can resolve.
        if let Some(default) = node
            .children()
            .find(|child| child.kind() == jals_syntax::SyntaxKind::ANNOTATION_DEFAULT)
        {
            let value =
                default
                    .children()
                    .find_map(ast::Expr::cast)
                    .ok_or(LowerError::Unsupported(
                        "an annotation default with no value",
                    ))?;
            let element = Self::element_value(
                &value,
                &context.index.resolved_member_ty(member),
                context,
                pool,
            )?;
            let name_index = pool
                .utf8_index("AnnotationDefault")
                .ok_or(AsmError::PoolFull)?;
            attributes.push(jals_classfile::Attribute {
                name_index,
                body: jals_classfile::AttributeBody::AnnotationDefault(element),
            });
        }
        // No body means no `Code` attribute, and the JVM accepts that only from a method whose flags
        // say why it has none. `native` says so with its own flag — already set above — and
        // `ACC_NATIVE | ACC_ABSTRACT` is a pair JVMS §4.6 forbids, which a JVM rejects with "illegal
        // modifiers: 0x500". `abstract` says so directly, and an interface method says so implicitly
        // (JLS §9.4). Anything else with no body is a declaration the JVM would refuse.
        let flags = if decl.body().is_none() && flags & MethodAccessFlags::NATIVE == 0 {
            if context.in_interface
                || Self::has_modifier(node, jals_syntax::SyntaxKind::ABSTRACT_KW)
            {
                flags | MethodAccessFlags::ABSTRACT
            } else {
                return Err(LowerError::Unsupported("a method with no body"));
            }
        } else {
            flags
        };

        Ok(MethodInfo {
            access_flags: MethodAccessFlags(flags),
            name_index,
            descriptor_index,
            attributes,
        })
    }

    /// One `element_value` (JVMS §4.7.16.1) for an annotation element's default.
    ///
    /// The tag comes from the element's *declared* type, not from the literal: `byte b() default 1`
    /// writes tag `B` over an `Integer` entry, and a reader that trusted the literal would see an `int`
    /// where a `byte` belongs. An enum constant, a class literal, and an array each have their own
    /// encoding, and the tag is what tells a reader which one it is looking at.
    fn element_value(
        value: &ast::Expr,
        declared: &jals_hir::Ty,
        context: &Context<'_>,
        pool: &mut ConstantPool,
    ) -> Result<jals_classfile::ElementValue> {
        use jals_hir::{Primitive, Ty};
        let unsupported = || LowerError::Unsupported("an annotation default of this form");
        let text = |node: &SyntaxNode| {
            node.children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .find(|token| !token.kind().is_trivia())
                .map(|token| token.text().to_owned())
        };
        match value {
            // `{…}` — every element at the *component* type, which is the only thing that says what tag
            // each of them carries. An empty one is legal and is what `default {}` means.
            ast::Expr::ArrayInit(init) => {
                let Ty::Array(element) = declared else {
                    return Err(LowerError::Unsupported(
                        "an annotation default array outside an array type",
                    ));
                };
                let mut out = Vec::new();
                for item in init.syntax().children().filter_map(ast::Expr::cast) {
                    out.push(Self::element_value(&item, element, context, pool)?);
                }
                return Ok(jals_classfile::ElementValue::Array(out));
            }
            // `T.class` — the tag is `c` and the payload is the *descriptor*, not the internal name.
            // The reference form's base is parsed as an expression, a bare `String` being a name
            // reference, so it is resolved as a type name rather than read as a `TYPE` node.
            ast::Expr::ClassLiteral(literal) => {
                let named = match (literal.ty(), literal.expr()) {
                    (Some(ty), _) => context.ty_of_type(&ty)?,
                    (None, Some(base)) => context.ty_of_name(base.syntax())?,
                    (None, None) => return Err(unsupported()),
                };
                let descriptor = Descriptor::descriptor_of(&named, context.index)?.to_string();
                return Ok(jals_classfile::ElementValue::Class {
                    class_info_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                });
            }
            // `Colour.RED` — an enum constant, named by the enum's descriptor and the constant's own
            // name. The *declared* type says which enum, for the same reason it says which tag.
            ast::Expr::FieldAccess(access) if matches!(declared, Ty::Class(_)) => {
                let name = access.field().ok_or_else(unsupported)?;
                let descriptor = Descriptor::descriptor_of(declared, context.index)?.to_string();
                return Ok(jals_classfile::ElementValue::Enum {
                    type_name_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                    const_name_index: pool.utf8_index(&name).ok_or(AsmError::PoolFull)?,
                });
            }
            _ => {}
        }
        let ast::Expr::Literal(literal) = value else {
            return Err(unsupported());
        };
        let literal = text(literal.syntax()).ok_or_else(unsupported)?;
        let integer = || {
            expr::Expr::integer_literal(literal.trim_end_matches(['l', 'L']))
                .map_err(|_| unsupported())
        };
        let floating = || {
            literal
                .trim_end_matches(['f', 'F', 'd', 'D'])
                .parse::<f64>()
                .map_err(|_| unsupported())
        };
        let (tag, const_value_index) = match declared {
            Ty::Primitive(Primitive::Boolean) => {
                let one = literal == "true";
                (b'Z', pool.integer_index(i32::from(one)))
            }
            Ty::Primitive(Primitive::Byte) => (b'B', pool.integer_index(Self::narrow(integer()?))),
            Ty::Primitive(Primitive::Short) => (b'S', pool.integer_index(Self::narrow(integer()?))),
            Ty::Primitive(Primitive::Char) => {
                let character = expr::Expr::literal_text(&literal)
                    .ok()
                    .and_then(|text| text.chars().next())
                    .ok_or_else(unsupported)?;
                (b'C', pool.integer_index(character as i32))
            }
            Ty::Primitive(Primitive::Int) => (b'I', pool.integer_index(Self::narrow(integer()?))),
            Ty::Primitive(Primitive::Long) => (b'J', pool.long_index(integer()?)),
            #[allow(clippy::cast_possible_truncation)]
            Ty::Primitive(Primitive::Float) => (b'F', pool.float_index(floating()? as f32)),
            Ty::Primitive(Primitive::Double) => (b'D', pool.double_index(floating()?)),
            Ty::Class(_) if expr::Expr::is_string(declared, context) => {
                let text = expr::Expr::literal_text(&literal).map_err(|_| unsupported())?;
                (b's', pool.utf8_index(&text))
            }
            _ => return Err(unsupported()),
        };
        Ok(jals_classfile::ElementValue::Const {
            tag,
            const_value_index: const_value_index.ok_or(AsmError::PoolFull)?,
        })
    }

    /// An `i64` literal as the `i32` a `B` / `S` / `C` / `I` element value holds.
    ///
    /// Out of range wraps, which is what a narrowing constant conversion does (JLS §5.2) — and what
    /// `jals-lint` reports, this crate not being the one that checks.
    #[allow(clippy::cast_possible_truncation)]
    const fn narrow(value: i64) -> i32 {
        value as i32
    }

    fn constructor(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
        super_name: &str,
        super_item: Option<ItemId>,
        members: &[SyntaxNode],
    ) -> Result<MethodInfo> {
        let name_token = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            .ok_or(LowerError::Unsupported("a malformed constructor"))?;
        let member = context.member_at(&name_token)?;
        let mut descriptor = Descriptor::method_descriptor(member, context.index, true)?;
        // An inner class's constructor takes the enclosing instance first: the index computed the
        // descriptor from the declaration, which does not write it.
        if let Some(enclosing) = &context.encloses {
            descriptor
                .params
                .insert(0, jals_classfile::FieldType::Object(enclosing.clone()));
        }
        // An `enum`'s takes the constant's name and ordinal first, for the same reason: they are what
        // `Enum`'s own constructor needs, and the declaration writes neither.
        if context.in_enum {
            descriptor.params.insert(
                0,
                jals_classfile::FieldType::Base(jals_classfile::BaseType::Int),
            );
            descriptor
                .params
                .insert(0, jals_classfile::FieldType::Object(STRING.to_owned()));
        }
        let synthetic = if context.in_enum {
            2
        } else {
            u16::from(context.encloses.is_some())
        };
        // The captures go *after* every declared parameter, so a declared one keeps its slot.
        for &captured in context.captured_here() {
            descriptor.params.push(Descriptor::descriptor_of(
                context.inference.type_of_def(captured),
                context.index,
            )?);
        }
        let text = MethodDescriptor::to_string(&descriptor);
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&text).ok_or(AsmError::PoolFull)?;

        let body = node.children().find_map(ast::Block::cast);
        let delegation = body
            .as_ref()
            .and_then(Self::explicit_constructor_invocation);
        // A `this(…)` between two `enum` constructors would be lowered from the descriptor the index
        // computed, which is two parameters short of the one emitted here — a `NoSuchMethodError` in
        // `<clinit>` rather than anything a verifier catches. (`super(…)` is not a Java program in an
        // `enum` at all: only `Enum` may call `Enum`'s constructor.)
        if context.in_enum && delegation.is_some() {
            return Err(LowerError::Unsupported(
                "an explicit constructor invocation in an `enum`",
            ));
        }

        let params = node.children().find_map(ast::ParamList::cast);
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), &text)?;
        let slots = Slots::for_constructor(context, params.as_ref(), synthetic);
        let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, true);
        match &delegation {
            // `this(…)` and `super(…)` each replace part of what the prologue emits, and running
            // both would run that part twice (JLS §8.8.7). `this(…)` replaces all of it — the
            // constructor it delegates to has already run the field initialisers — while `super(…)`
            // replaces only the `super()` call, so the initialisers still follow it.
            Some(call) => {
                expr::Expr::lower(&ast::Expr::Call(call.clone()), context, &mut emit)?;
                if !Self::delegates_to_this(call) {
                    Self::initializers(context, &mut emit, members, false)?;
                }
            }
            None => Self::prologue(context, &mut emit, super_name, super_item, members, None)?,
        }
        if let Some(body) = &body {
            // The explicit invocation is the body's first statement, and it has already been emitted.
            for statement in body.stmts().skip(usize::from(delegation.is_some())) {
                stmt::Stmt::lower(&statement, context, &mut emit)?;
            }
        }
        // A constructor is `void`, so it needs the instruction unless every path already returned.
        if asm.reachable() {
            asm.return_(None)?;
        }

        Ok(MethodInfo {
            access_flags: MethodAccessFlags(Self::method_flags(node, context.in_interface)),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// The `Signature` a generic type declaration needs, or `None` when it declares no type parameters.
    ///
    /// JVMS §4.7.9.1's `ClassSignature`: the type parameters with their bounds, then the superclass, then
    /// each superinterface. The supertypes are written in their *erased* form here — a generic supertype
    /// (`class Box<T> extends Holder<T>`) would need the arguments the `extends` clause wrote, and
    /// erasing them loses only what a reflective reader would see, never what the JVM links on.
    fn class_signature(
        node: &SyntaxNode,
        context: &Context<'_>,
        super_name: &str,
        interface_names: &[String],
    ) -> Result<Option<String>> {
        let Some(params) = node.children().find_map(ast::TypeParams::cast) else {
            return Ok(None);
        };
        let declared: Vec<ast::TypeParam> = params.params().collect();
        if declared.is_empty() {
            return Ok(None);
        }
        let mut out = String::from("<");
        for param in &declared {
            let name = param
                .name()
                .ok_or(LowerError::Unsupported("a type parameter with no name"))?;
            out.push_str(&name);
            // A parameter with no `extends` is bounded by `Object`, and the bound is not optional in the
            // encoding: `<T>` is written `<T:Ljava/lang/Object;>`.
            let bounds: Vec<ast::Type> = param
                .syntax()
                .children()
                .filter_map(ast::Type::cast)
                .collect();
            if bounds.is_empty() {
                out.push_str(":Ljava/lang/Object;");
                continue;
            }
            for (position, bound) in bounds.iter().enumerate() {
                // The first bound may be a class or an interface, and the encoding does not distinguish:
                // `:` introduces the class bound and each further `:` an interface bound.
                // `:` introduces the class bound and each further `:` an interface bound, so one per
                // bound is the whole rule and the position does not change it.
                let _ = position;
                out.push(':');
                let ty = context.ty_of_type(bound)?;
                out.push_str(&Descriptor::descriptor_of(&ty, context.index)?.to_string());
            }
        }
        out.push('>');
        // The *written* supertypes, so a generic one keeps its arguments (`extends Holder<T>`). A class
        // with no `extends` has none to write and gets `Object`.
        let vars: Vec<String> = declared.iter().filter_map(ast::TypeParam::name).collect();
        let written = |kind: jals_syntax::SyntaxKind| -> Vec<ast::Type> {
            node.children()
                .filter(|child| child.kind() == kind)
                .flat_map(|clause| clause.children().filter_map(ast::Type::cast))
                .collect()
        };
        if let Some(ty) = written(jals_syntax::SyntaxKind::EXTENDS_CLAUSE).first() {
            out.push_str(&Self::type_signature(ty, &vars, context)?);
        } else {
            out.push('L');
            out.push_str(super_name);
            out.push(';');
        }
        let implemented = written(jals_syntax::SyntaxKind::IMPLEMENTS_CLAUSE);
        if implemented.len() == interface_names.len() {
            for ty in &implemented {
                out.push_str(&Self::type_signature(ty, &vars, context)?);
            }
        } else {
            // An interface the source wrote that the index did not resolve would put the two lists out
            // of step; the erased names are still right, only less informative.
            for name in interface_names {
                out.push('L');
                out.push_str(name);
                out.push(';');
            }
        }
        Ok(Some(out))
    }

    /// The `MethodSignature` (JVMS §4.7.9.1) a generic method needs, or `None` when nothing about it is
    /// generic.
    ///
    /// Its own type parameters first, then each formal parameter, then the result. `throws` is written
    /// only when a thrown type is itself a variable, which is the one case the encoding requires it.
    fn method_signature(node: &SyntaxNode, context: &Context<'_>) -> Result<Option<String>> {
        let decl = ast::MethodDecl::cast(node.clone())
            .ok_or(LowerError::Unsupported("a malformed method declaration"))?;
        let vars = Self::type_variables(node);
        let own: Vec<ast::TypeParam> = node
            .children()
            .find_map(ast::TypeParams::cast)
            .map(|params| params.params().collect())
            .unwrap_or_default();
        let parameters: Vec<ast::Type> = decl
            .params()
            .map(|list| list.params().filter_map(|param| param.ty()).collect())
            .unwrap_or_default();
        let returns = decl.return_type();
        let generic = !own.is_empty()
            || parameters
                .iter()
                .chain(returns.as_ref())
                .any(|ty| Self::mentions_variable(ty, &vars));
        if !generic {
            return Ok(None);
        }
        let mut out = String::new();
        if !own.is_empty() {
            out.push('<');
            for param in &own {
                let name = param
                    .name()
                    .ok_or(LowerError::Unsupported("a type parameter with no name"))?;
                out.push_str(&name);
                let bounds: Vec<ast::Type> = param
                    .syntax()
                    .children()
                    .filter_map(ast::Type::cast)
                    .collect();
                if bounds.is_empty() {
                    out.push_str(":Ljava/lang/Object;");
                    continue;
                }
                for bound in &bounds {
                    out.push(':');
                    out.push_str(&Self::type_signature(bound, &vars, context)?);
                }
            }
            out.push('>');
        }
        out.push('(');
        for ty in &parameters {
            out.push_str(&Self::type_signature(ty, &vars, context)?);
        }
        out.push(')');
        match &returns {
            // `void` has no reference signature; the encoding spells it `V` like a descriptor does.
            Some(ty)
                if !ty
                    .syntax()
                    .children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .any(|token| token.kind() == jals_syntax::SyntaxKind::VOID_KW) =>
            {
                out.push_str(&Self::type_signature(ty, &vars, context)?);
            }
            _ => out.push('V'),
        }
        // A thrown *type variable* is the one case the encoding needs a `throws` part for: an ordinary
        // thrown class is already in the `Exceptions` attribute and adds nothing here.
        let thrown: Vec<ast::Type> = node
            .children()
            .filter(|child| child.kind() == jals_syntax::SyntaxKind::THROWS_CLAUSE)
            .flat_map(|clause| clause.children().filter_map(ast::Type::cast))
            .collect();
        if thrown.iter().any(|ty| Self::mentions_variable(ty, &vars)) {
            for ty in &thrown {
                out.push('^');
                out.push_str(&Self::type_signature(ty, &vars, context)?);
            }
        }
        Ok(Some(out))
    }

    /// The type-variable names in scope for a member of `node`'s enclosing declaration, plus any the
    /// member itself declares.
    fn type_variables(member: &SyntaxNode) -> Vec<String> {
        let mut names = Vec::new();
        for ancestor in member.ancestors() {
            if let Some(params) = ancestor.children().find_map(ast::TypeParams::cast) {
                names.extend(params.params().filter_map(|param| param.name()));
            }
        }
        names
    }

    /// One `JavaTypeSignature` (JVMS §4.7.9.1) for a written type.
    ///
    /// A name in `vars` is a *type variable*, written `T<name>;` — which is the whole reason this exists
    /// rather than reusing the descriptor: erasure replaces a variable with its bound, and the signature
    /// is where the variable itself is kept. Type *arguments* are erased here for the same reason the
    /// class signature erases its supertypes: leaving them out loses only what a reflective reader would
    /// see, never what the JVM links on.
    fn type_signature(ty: &ast::Type, vars: &[String], context: &Context<'_>) -> Result<String> {
        use jals_syntax::SyntaxKind::{LBRACK, TYPE_ARGS};
        let dimensions = ty
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == LBRACK)
            .count();
        let mut out = "[".repeat(dimensions);
        if let Some(name) = ty.simple_name().filter(|name| {
            !ty.is_qualified() && vars.iter().any(|var| var == name) && !ty.is_primitive_or_var()
        }) {
            out.push('T');
            out.push_str(&name);
            out.push(';');
            return Ok(out);
        }
        // Not a variable: the erased descriptor for the name, plus the type *arguments* the source
        // wrote. A reflective reader gets `List<T>` from this where the descriptor alone says `List`.
        let erased = context.ty_of_type(ty)?;
        let descriptor = Descriptor::descriptor_of(&erased, context.index)?.to_string();
        let name = descriptor.trim_start_matches('[');
        let Some(args) = ty
            .syntax()
            .children()
            .find(|child| child.kind() == TYPE_ARGS)
        else {
            out.push_str(name);
            return Ok(out);
        };
        let rendered = Self::argument_signatures(&args, vars, context)?;
        if rendered.is_empty() || !name.starts_with('L') {
            out.push_str(name);
            return Ok(out);
        }
        // `Lname<args>;` — the arguments go before the terminating semicolon, not after it.
        out.push_str(name.trim_end_matches(';'));
        out.push('<');
        for argument in &rendered {
            out.push_str(argument);
        }
        out.push_str(">;");
        Ok(out)
    }

    /// One `TypeArgument` per argument in a `TYPE_ARGS` node, in order.
    ///
    /// A wildcard is *not* wrapped in a type node of its own: its `?`, its `extends` / `super`, and its
    /// bound are all direct children of the argument list. So the list is walked in order rather than
    /// through the typed accessor, which skips the `?` and would render `? extends T` as plain `T`.
    fn argument_signatures(
        args: &SyntaxNode,
        vars: &[String],
        context: &Context<'_>,
    ) -> Result<Vec<String>> {
        use jals_syntax::SyntaxKind::{EXTENDS_KW, QUESTION, SUPER_KW};
        let mut out = Vec::new();
        let mut pending: Option<char> = None;
        for child in args.children_with_tokens() {
            match child {
                jals_syntax::SyntaxElement::Token(token) => match token.kind() {
                    // A `?` with no bound is `*`, which only the *next* element can tell us: flush it
                    // when the argument ends rather than guessing here.
                    QUESTION => {
                        if pending.is_some() {
                            out.push("*".to_owned());
                        }
                        pending = Some('*');
                    }
                    EXTENDS_KW if pending.is_some() => pending = Some('+'),
                    SUPER_KW if pending.is_some() => pending = Some('-'),
                    jals_syntax::SyntaxKind::COMMA | jals_syntax::SyntaxKind::GT
                        if pending == Some('*') =>
                    {
                        pending = None;
                        out.push("*".to_owned());
                    }
                    _ => {}
                },
                jals_syntax::SyntaxElement::Node(node) => {
                    let Some(ty) = ast::Type::cast(node) else {
                        continue;
                    };
                    let rendered = Self::type_signature(&ty, vars, context)?;
                    match pending.take() {
                        Some('+') => out.push(alloc::format!("+{rendered}")),
                        Some('-') => out.push(alloc::format!("-{rendered}")),
                        // A `?` immediately followed by a type with no keyword between cannot happen in
                        // a well-formed program; treat the `?` as unbounded and the type as its own.
                        Some(_) => {
                            out.push("*".to_owned());
                            out.push(rendered);
                        }
                        None => out.push(rendered),
                    }
                }
            }
        }
        if pending.is_some() {
            out.push("*".to_owned());
        }
        Ok(out)
    }

    /// Whether a written type mentions any of `vars`, which is what decides if a member needs a
    /// `Signature` at all.
    fn mentions_variable(ty: &ast::Type, vars: &[String]) -> bool {
        ty.syntax()
            .descendants_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| match token.kind() {
                jals_syntax::SyntaxKind::IDENT => vars.iter().any(|var| var == token.text()),
                // A wildcard names no variable and still needs a signature: `Holder<?>` erases to
                // `Holder`, and the `?` exists nowhere else.
                jals_syntax::SyntaxKind::QUESTION => true,
                _ => false,
            })
    }

    /// A record's synthesised members: a `private final` field per component, the canonical
    /// constructor, an accessor per component, and the `Record` attribute's component list.
    ///
    /// Every one of them comes from the *header*, which is the only place a component is written — so a
    /// record that emitted just what its body declares would be a type with no state, no way to build
    /// one, and no way to read one. An accessor the body declares by hand wins over the synthesised one
    /// (JLS §8.10.3); so does an explicit canonical constructor.
    ///
    /// `equals`, `hashCode`, and `toString` are **required from the source** here. `java.lang.Record`
    /// declares all three abstract, so a class file that omits any of them loads and then throws
    /// `AbstractMethodError` at the first call — which is why this reports rather than emits nothing.
    fn record_members(
        node: &SyntaxNode,
        context: &Context<'_>,
        pool: &mut ConstantPool,
        members: &[SyntaxNode],
        super_name: &str,
        fields: &mut Vec<FieldInfo>,
        methods: &mut Vec<MethodInfo>,
    ) -> Result<Vec<jals_classfile::RecordComponentInfo>> {
        let components = Self::record_components(node);
        let mut infos = Vec::with_capacity(components.len());
        let mut descriptors = Vec::with_capacity(components.len());
        for name in &components {
            let member = context.member_at(name)?;
            let descriptor = Descriptor::field_descriptor(member, context.index)?.to_string();
            // `private final`, which is what makes a record's state immutable and reachable only
            // through the accessors.
            fields.push(FieldInfo {
                access_flags: FieldAccessFlags(FieldAccessFlags::PRIVATE | FieldAccessFlags::FINAL),
                name_index: pool.utf8_index(name.text()).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
            });
            infos.push(jals_classfile::RecordComponentInfo {
                name_index: pool.utf8_index(name.text()).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
            });
            descriptors.push((name.text().to_owned(), descriptor));
        }

        // A *compact* constructor (`P { … }`, with no parameter list at all) **is** the canonical one:
        // it takes the components implicitly, and the field assignments follow its body. So it is
        // emitted from here rather than from the member loop, which would give it `<init>()V`.
        let compact = members
            .iter()
            .find(|member| Self::is_compact_constructor(member));
        // Only an explicit *canonical* constructor replaces the synthesised one — "some constructor
        // exists" is not the same thing. `record P(int x) { P() { this(0); } }` declares a
        // no-argument one and still needs `<init>(I)V` for `this(0)` to have a target.
        let declares_canonical = members.iter().any(|member| {
            member.kind() == CONSTRUCTOR_DECL
                && member
                    .children()
                    .find_map(ast::ParamList::cast)
                    .map_or(0, |list| list.params().count())
                    == components.len()
        });
        if compact.is_some() || !declares_canonical {
            methods.push(Self::canonical_constructor(
                context,
                pool,
                super_name,
                &descriptors,
                Self::access_level(node),
                compact.map(|member| (member, components.as_slice())),
            )?);
        }
        for (name, descriptor) in &descriptors {
            // An accessor the body declares by hand is already in `methods`; the synthesised one would
            // be a duplicate the JVM rejects outright.
            let declared = members.iter().any(|member| {
                member.kind() == METHOD_DECL
                    && ast::MethodDecl::cast(member.clone())
                        .and_then(|decl| decl.name())
                        .as_deref()
                        == Some(name.as_str())
            });
            if declared {
                continue;
            }
            methods.push(Self::record_accessor(context, pool, name, descriptor)?);
        }
        // `java.lang.Record` declares all three abstract, so a record without them loads perfectly and
        // then throws `AbstractMethodError` at the first call. javac derives them through
        // `invokedynamic` to `ObjectMethods.bootstrap`; written out by hand they are the same three
        // methods, and §8.10.3 leaves `hashCode`'s algorithm unspecified so any consistent one is legal.
        let declared = |name: &str| {
            members.iter().any(|member| {
                member.kind() == METHOD_DECL
                    && ast::MethodDecl::cast(member.clone())
                        .and_then(|decl| decl.name())
                        .as_deref()
                        == Some(name)
            })
        };
        if !declared("equals") {
            methods.push(Self::record_equals(context, pool, &descriptors)?);
        }
        if !declared("hashCode") {
            methods.push(Self::record_hash(context, pool, &descriptors)?);
        }
        if !declared("toString") {
            methods.push(Self::record_string(context, pool, &descriptors)?);
        }
        Ok(infos)
    }

    /// `equals(Object)`: identity, then the type, then every component in header order.
    ///
    /// A `float` or `double` component compares with `Float.compare(a, b) == 0` rather than `==`, which
    /// is what makes two `NaN` components equal and `0.0` and `-0.0` different (§8.10.3). A reference
    /// component goes through `Objects.equals`, which is null-safe — `==` would be identity and a bare
    /// `a.equals(b)` would throw on a `null` component.
    fn record_equals(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        components: &[(String, String)],
    ) -> Result<MethodInfo> {
        const DESCRIPTOR: &str = "(Ljava/lang/Object;)Z";
        let name_index = pool.utf8_index("equals").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(DESCRIPTOR).ok_or(AsmError::PoolFull)?;
        let owner = context.this_class.clone();
        let mut asm = Assembler::new(pool, Receiver::Instance(&owner), DESCRIPTOR)?;
        let same = asm.label();
        let different = asm.label();

        // `this == other` short-circuits, which is both faster and what every hand-written `equals`
        // starts with.
        asm.load(0)?;
        asm.load(1)?;
        asm.branch(Branch::RefSame(true), same)?;
        asm.load(1)?;
        asm.instance_of(&owner)?;
        asm.branch(Branch::IntZero(Compare::Eq), different)?;
        // The cast is safe past the `instanceof`, and the local holds it so each component reads the
        // narrowed reference rather than casting again.
        let other = u16::try_from(components.len() + 2).unwrap_or(u16::MAX);
        asm.load(1)?;
        asm.check_cast(&owner)?;
        asm.store(other)?;

        for (name, descriptor) in components {
            asm.load(0)?;
            asm.get_field(&owner, name, descriptor)?;
            asm.load(other)?;
            asm.get_field(&owner, name, descriptor)?;
            match descriptor.as_bytes() {
                [b'Z' | b'B' | b'S' | b'C' | b'I'] => {
                    asm.branch(Branch::IntCmp(Compare::Ne), different)?;
                }
                // `branch_compare` owns the `lcmp` and the branch together, which is also where the
                // NaN direction rule lives for the floating types it is not used for here.
                [b'J'] => {
                    asm.branch_compare(&VerificationType::Long, Compare::Ne, different)?;
                }
                [b'F'] => {
                    asm.invoke_static("java/lang/Float", "compare", "(FF)I", false)?;
                    asm.branch(Branch::IntZero(Compare::Ne), different)?;
                }
                [b'D'] => {
                    asm.invoke_static("java/lang/Double", "compare", "(DD)I", false)?;
                    asm.branch(Branch::IntZero(Compare::Ne), different)?;
                }
                _ => {
                    asm.invoke_static(
                        "java/util/Objects",
                        "equals",
                        "(Ljava/lang/Object;Ljava/lang/Object;)Z",
                        false,
                    )?;
                    asm.branch(Branch::IntZero(Compare::Eq), different)?;
                }
            }
        }

        asm.bind(same)?;
        asm.const_int(1)?;
        asm.return_(Some(&VerificationType::Integer))?;
        asm.bind(different)?;
        asm.const_int(0)?;
        asm.return_(Some(&VerificationType::Integer))?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// `hashCode()`: `31 * h + component`, folded over the components in header order.
    ///
    /// §8.10.3 leaves the algorithm unspecified — only that it is derived from the components — so any
    /// consistent one is legal, and this is the one every hand-written `hashCode` uses.
    fn record_hash(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        components: &[(String, String)],
    ) -> Result<MethodInfo> {
        let name_index = pool.utf8_index("hashCode").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index("()I").ok_or(AsmError::PoolFull)?;
        let owner = context.this_class.clone();
        let mut asm = Assembler::new(pool, Receiver::Instance(&owner), "()I")?;
        asm.const_int(0)?;
        for (name, descriptor) in components {
            asm.const_int(31)?;
            asm.binary(BinOp::Mul, &VerificationType::Integer)?;
            asm.load(0)?;
            asm.get_field(&owner, name, descriptor)?;
            Self::component_hash(&mut asm, descriptor)?;
            asm.binary(BinOp::Add, &VerificationType::Integer)?;
        }
        asm.return_(Some(&VerificationType::Integer))?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// Reduce one component's value, already on the stack, to the `int` its hash contributes.
    ///
    /// Each primitive gets the reduction its wrapper's `hashCode` uses, because two values that are
    /// `equals` must hash alike: a `long` folds its halves together, and a `float` hashes its *bits* so
    /// that two `NaN`s — which a record's `equals` calls equal — agree.
    fn component_hash(asm: &mut Assembler<'_>, descriptor: &str) -> Result<()> {
        match descriptor.as_bytes() {
            // A `boolean` is already 0 or 1 on the stack, and `Boolean.hashCode` maps those to
            // 1237 and 1231. Multiplying is how the two constants are reached without a branch.
            [b'Z'] => {
                asm.const_int(-6)?;
                asm.binary(BinOp::Mul, &VerificationType::Integer)?;
                asm.const_int(1237)?;
                asm.binary(BinOp::Add, &VerificationType::Integer)?;
            }
            [b'B' | b'S' | b'C' | b'I'] => {}
            [b'J'] => Self::fold_long(asm)?,
            [b'F'] => asm.invoke_static("java/lang/Float", "floatToIntBits", "(F)I", false)?,
            [b'D'] => {
                asm.invoke_static("java/lang/Double", "doubleToLongBits", "(D)J", false)?;
                Self::fold_long(asm)?;
            }
            // Null-safe, unlike a bare `hashCode()` call on a component that may be `null`.
            _ => asm.invoke_static(
                "java/util/Objects",
                "hashCode",
                "(Ljava/lang/Object;)I",
                false,
            )?,
        }
        Ok(())
    }

    /// `(int) (value ^ (value >>> 32))` over the `long` on top: `Long.hashCode`'s own reduction.
    fn fold_long(asm: &mut Assembler<'_>) -> Result<()> {
        // `dup` on a `long` is `dup2`; `dup_pair` is for two *separate* one-word values.
        asm.dup()?;
        asm.const_int(32)?;
        asm.binary(BinOp::Ushr, &VerificationType::Long)?;
        asm.binary(BinOp::Xor, &VerificationType::Long)?;
        asm.convert(Numeric::Long, Numeric::Int)?;
        Ok(())
    }

    /// `toString()`: `Name[a=1, b=2]`, which §8.10.3 *does* specify exactly.
    ///
    /// A `StringBuilder` chain rather than `invokedynamic`, the same way `+` on a `String` lowers.
    fn record_string(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        components: &[(String, String)],
    ) -> Result<MethodInfo> {
        const BUILDER: &str = "java/lang/StringBuilder";
        const DESCRIPTOR: &str = "()Ljava/lang/String;";
        let name_index = pool.utf8_index("toString").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(DESCRIPTOR).ok_or(AsmError::PoolFull)?;
        let owner = context.this_class.clone();
        // The *simple* name, which is what the specified format uses — `Outer$Inner` prints as `Inner`.
        let simple = owner.rsplit(['/', '$']).next().unwrap_or(&owner).to_owned();
        let mut asm = Assembler::new(pool, Receiver::Instance(&owner), DESCRIPTOR)?;
        asm.new_object(BUILDER)?;
        asm.dup()?;
        asm.invoke_special(BUILDER, "<init>", "()V", false)?;
        let mut literal = alloc::format!("{simple}[");
        for (index, (name, descriptor)) in components.iter().enumerate() {
            if index > 0 {
                literal.push_str(", ");
            }
            literal.push_str(name);
            literal.push('=');
            asm.const_string(&literal)?;
            asm.invoke_virtual(
                BUILDER,
                "append",
                "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
            )?;
            literal.clear();
            asm.load(0)?;
            asm.get_field(&owner, name, descriptor)?;
            asm.invoke_virtual(BUILDER, "append", &Self::append_descriptor(descriptor))?;
        }
        literal.push(']');
        asm.const_string(&literal)?;
        asm.invoke_virtual(
            BUILDER,
            "append",
            "(Ljava/lang/String;)Ljava/lang/StringBuilder;",
        )?;
        asm.invoke_virtual(BUILDER, "toString", DESCRIPTOR)?;
        let top = asm
            .stack_top()
            .ok_or(LowerError::Unsupported("a `toString` that built nothing"))?;
        asm.return_(Some(&top))?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// The `StringBuilder.append` overload a component of this descriptor selects.
    ///
    /// There is no `append(byte)` or `append(short)`: both widen to `int`, which is what the JVM's
    /// stack already holds them as.
    fn append_descriptor(descriptor: &str) -> alloc::string::String {
        let parameter = match descriptor.as_bytes() {
            [b'Z'] => "Z",
            [b'C'] => "C",
            [b'B' | b'S' | b'I'] => "I",
            [b'J'] => "J",
            [b'F'] => "F",
            [b'D'] => "D",
            _ => "Ljava/lang/Object;",
        };
        alloc::format!("({parameter})Ljava/lang/StringBuilder;")
    }

    /// The name token of every component in a record's header, in order.
    fn record_components(node: &SyntaxNode) -> Vec<SyntaxToken> {
        node.children()
            .find(|child| child.kind() == jals_syntax::SyntaxKind::RECORD_HEADER)
            .into_iter()
            .flat_map(|header| header.children())
            .filter(|child| child.kind() == jals_syntax::SyntaxKind::RECORD_COMPONENT)
            .filter_map(|component| {
                component
                    .children_with_tokens()
                    .filter_map(jals_syntax::SyntaxElement::into_token)
                    .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)
            })
            .collect()
    }

    /// The canonical constructor: `super()`, the compact body if there is one, then one `putfield` per
    /// component, in header order.
    ///
    /// A *compact* constructor (`P { … }`) declares no parameters and no assignments, and means both:
    /// the components are its parameters, and the field writes follow whatever it did to them
    /// (JLS §8.10.4.2). So the body is lowered with each component's *definition* bound to the
    /// parameter slot rather than to the field — which is what makes `x = Math.abs(x)` normalise the
    /// argument and what leaves the field still zero for the body's own reads, as Java has it.
    fn canonical_constructor(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        super_name: &str,
        components: &[(String, String)],
        access: u16,
        compact: Option<(&SyntaxNode, &[SyntaxToken])>,
    ) -> Result<MethodInfo> {
        let mut descriptor = String::from("(");
        for (_, component) in components {
            descriptor.push_str(component);
        }
        descriptor.push_str(")V");
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(
            pool,
            Receiver::Constructor(&context.this_class),
            &descriptor,
        )?;
        asm.load(0)?;
        asm.invoke_special(super_name, "<init>", "()V", false)?;

        // Parameter 0 is `this`; each component's slot follows, at its own width — a `long` or a
        // `double` component takes two, and getting that wrong reads the *next* parameter's low half.
        let mut slots = Slots::for_constructor(context, None, 0);
        let mut placements = Vec::with_capacity(components.len());
        for (position, (_, component)) in components.iter().enumerate() {
            let width = Slots::descriptor_width(component);
            // The definition a body reference to the component's name binds to. Declaring it here is
            // what puts the *parameter* ahead of the field in `Place::resolve` and in `Expr::name`,
            // both of which consult the slot map before falling back to a field of the enclosing type.
            let bound = compact
                .and_then(|(_, names)| names.get(position))
                .and_then(SyntaxToken::parent)
                .and_then(|node| context.def_at(&node));
            placements.push(match bound {
                Some(id) => slots.declare(id, width),
                None => slots.declare_temporary(width),
            });
        }

        let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, true);
        if let Some((member, _)) = compact {
            let body = member
                .children()
                .find_map(ast::Block::cast)
                .ok_or(LowerError::Unsupported("a malformed constructor"))?;
            // A `return` would jump over the field writes, leaving every component at its default. JLS
            // §8.10.4.2 makes one a compile-time error for exactly that reason, so there is no correct
            // lowering to pick — reported rather than emitted with the writes skipped. A `return` inside
            // a lambda in the body is caught too, which over-reports rather than under-reports.
            if body
                .syntax()
                .descendants()
                .any(|node| node.kind() == jals_syntax::SyntaxKind::RETURN_STMT)
            {
                return Err(LowerError::Unsupported(
                    "a `return` in a compact `record` constructor",
                ));
            }
            for statement in body.stmts() {
                stmt::Stmt::lower(&statement, context, &mut emit)?;
            }
        }
        for ((name, component), slot) in components.iter().zip(placements) {
            emit.asm.load(0)?;
            emit.asm.load(slot)?;
            emit.asm.put_field(&context.this_class, name, component)?;
        }
        emit.asm.return_(None)?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(access),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// Whether a member is a record's *compact* constructor: a `CONSTRUCTOR_DECL` with no parameter
    /// list at all, which is the only thing that distinguishes `P { … }` from `P() { … }` in the tree.
    fn is_compact_constructor(member: &SyntaxNode) -> bool {
        member.kind() == CONSTRUCTOR_DECL
            && !member
                .children()
                .any(|child| child.kind() == jals_syntax::SyntaxKind::PARAM_LIST)
    }

    /// One accessor: `return this.name;`.
    fn record_accessor(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        name: &str,
        descriptor: &str,
    ) -> Result<MethodInfo> {
        let signature = alloc::format!("(){descriptor}");
        let name_index = pool.utf8_index(name).ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&signature).ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Instance(&context.this_class), &signature)?;
        asm.load(0)?;
        asm.get_field(&context.this_class, name, descriptor)?;
        let top = asm
            .stack_top()
            .ok_or(LowerError::Unsupported("an accessor that read nothing"))?;
        asm.return_(Some(&top))?;
        Ok(MethodInfo {
            // An accessor is `public` whatever the record's own access level is (JLS §8.10.3).
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// The constructor a class with none of its own gets. `access` is the *class's* access level,
    /// which JLS §8.8.9 gives the default constructor — a `public` one on a package-private class
    /// would widen the type's reachable surface past what the source wrote.
    fn default_constructor(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        super_name: &str,
        super_item: Option<ItemId>,
        members: &[SyntaxNode],
        access: u16,
        forwarded: Option<&MethodDescriptor>,
    ) -> Result<MethodInfo> {
        let mut params = String::new();
        if let Some(enclosing) = &context.encloses {
            params.push('L');
            params.push_str(enclosing);
            params.push(';');
        }
        // An anonymous class's own constructor takes the superclass constructor's parameters and passes
        // them on: `new Base(1) { … }` has nowhere else to say `super(1)` from, the body declaring no
        // constructor. They go before the captures, which is where a declared constructor's own are.
        let mut synthetic = u16::from(context.encloses.is_some());
        for param in forwarded.iter().flat_map(|descriptor| &descriptor.params) {
            let written = param.to_string();
            synthetic += Slots::descriptor_width(&written);
            params.push_str(&written);
        }
        for &captured in context.captured_here() {
            params.push_str(&Self::capture_descriptor(captured, context)?);
        }
        let text = alloc::format!("({params})V");
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&text).ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), &text)?;
        let slots = Slots::for_constructor(context, None, synthetic);
        let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, true);
        Self::prologue(
            context, &mut emit, super_name, super_item, members, forwarded,
        )?;
        asm.return_(None)?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(access),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// The one `enum` shape this reports.
    ///
    /// A constant with a **body** is an anonymous subclass: its own class file, with the enum then not
    /// `final` and the constant's field typed as the base. Nothing else about the declaration says so,
    /// which is why it is checked here rather than found by whatever tried to emit it.
    fn enum_shape(constants: &[ast::EnumConstant]) -> Result<()> {
        if constants.iter().any(|constant| constant.body().is_some()) {
            return Err(LowerError::Unsupported("an `enum` constant with a body"));
        }
        Ok(())
    }

    /// An `enum`'s four synthesised member groups: a field per constant, the `$VALUES` array, the
    /// constructor, and `values()` / `valueOf()`.
    ///
    /// None of them is written in the source and every one is required — `Enum.valueOf` reads the
    /// constants reflectively, a `switch` over the type reads `ordinal()`, and `values()` is how a
    /// caller enumerates them. So an `enum` that emitted only what its body declares would be a type
    /// with no constants at all.
    ///
    /// Two shapes are reported. A constant with **arguments** or a **declared constructor** both need
    /// the two synthetic parameters (`name`, `ordinal`) *prepended* to a descriptor the index computed
    /// from the declaration, which would leave every one of them two parameters short; a constant with
    /// a **body** is an anonymous subclass, which is a separate class file the enum then cannot be
    /// `final`.
    fn enum_members(
        constants: &[ast::EnumConstant],
        context: &Context<'_>,
        pool: &mut ConstantPool,
        internal_name: &str,
        members: &[SyntaxNode],
        fields: &mut Vec<FieldInfo>,
        methods: &mut Vec<MethodInfo>,
    ) -> Result<()> {
        let descriptor = alloc::format!("L{internal_name};");
        let array = alloc::format!("[{descriptor}");
        for constant in constants {
            let name = constant
                .name()
                .ok_or(LowerError::Unsupported("an `enum` constant with no name"))?;
            fields.push(FieldInfo {
                access_flags: FieldAccessFlags(
                    FieldAccessFlags::PUBLIC
                        | FieldAccessFlags::STATIC
                        | FieldAccessFlags::FINAL
                        | FieldAccessFlags::ENUM,
                ),
                name_index: pool.utf8_index(&name).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
            });
        }
        fields.push(FieldInfo {
            access_flags: FieldAccessFlags(
                FieldAccessFlags::PRIVATE
                    | FieldAccessFlags::STATIC
                    | FieldAccessFlags::FINAL
                    | FieldAccessFlags::SYNTHETIC,
            ),
            name_index: pool.utf8_index(VALUES).ok_or(AsmError::PoolFull)?,
            descriptor_index: pool.utf8_index(&array).ok_or(AsmError::PoolFull)?,
            attributes: Vec::new(),
        });

        // `private Color(String name, int ordinal) { super(name, ordinal); <initialisers> }`
        //
        // Only when the source declares none: a declared one is emitted from the member loop, with the
        // same two synthetic parameters ahead of whatever it wrote, and it runs the initialisers itself.
        if !members
            .iter()
            .any(|member| member.kind() == CONSTRUCTOR_DECL)
        {
            let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
            let descriptor_index = pool.utf8_index(ENUM_INIT).ok_or(AsmError::PoolFull)?;
            let mut asm = Assembler::new(pool, Receiver::Constructor(internal_name), ENUM_INIT)?;
            let slots = Slots::new(context, None, false);
            let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, true);
            emit.asm.load(0)?;
            emit.asm.load(1)?;
            emit.asm.load(2)?;
            emit.asm.invoke_special(ENUM, "<init>", ENUM_INIT, false)?;
            Self::initializers(context, &mut emit, members, false)?;
            asm.return_(None)?;
            methods.push(MethodInfo {
                access_flags: MethodAccessFlags(MethodAccessFlags::PRIVATE),
                name_index,
                descriptor_index,
                attributes: alloc::vec![asm.finish()?],
            });
        }

        // `public static Color[] values() { return (Color[]) $VALUES.clone(); }`
        //
        // A *clone*, so a caller cannot reorder the constants through the array it was handed. `clone`
        // on an array type is declared to return `Object`, hence the cast.
        let values_descriptor = alloc::format!("(){array}");
        let name_index = pool.utf8_index("values").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool
            .utf8_index(&values_descriptor)
            .ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Static, &values_descriptor)?;
        asm.get_static(internal_name, VALUES, &array)?;
        asm.invoke_virtual(&array, "clone", "()Ljava/lang/Object;")?;
        asm.check_cast(&array)?;
        let returned = asm.stack_top().ok_or(AsmError::StackUnderflow)?;
        asm.return_(Some(&returned))?;
        methods.push(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC | MethodAccessFlags::STATIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        });

        // `public static Color valueOf(String name) { return (Color) Enum.valueOf(Color.class, name); }`
        let of_descriptor = alloc::format!("(Ljava/lang/String;){descriptor}");
        let name_index = pool.utf8_index("valueOf").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&of_descriptor).ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Static, &of_descriptor)?;
        asm.const_class(internal_name)?;
        asm.load(0)?;
        asm.invoke_static(
            ENUM,
            "valueOf",
            "(Ljava/lang/Class;Ljava/lang/String;)Ljava/lang/Enum;",
            false,
        )?;
        asm.check_cast(internal_name)?;
        let returned = asm.stack_top().ok_or(AsmError::StackUnderflow)?;
        asm.return_(Some(&returned))?;
        methods.push(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::PUBLIC | MethodAccessFlags::STATIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        });
        Ok(())
    }

    /// The declared `enum` constructor a constant with `arity` arguments builds through.
    ///
    /// `None` when the source declares none, which is the synthesised `(String, int)` one. Selected by
    /// arity rather than by applicability: a constant's argument list is not an expression the index
    /// resolved a call target for, so there is nothing to read a selection out of. Two constructors of
    /// the same arity are reported instead of guessed at.
    fn enum_constructor(arity: usize, context: &Context<'_>) -> Result<Option<jals_hir::MemberId>> {
        let mut matching = context
            .index
            .own_members(context.this_item)
            .iter()
            .copied()
            .filter(|&member| {
                let info = context.index.member(member);
                info.kind == DefKind::Constructor && info.params.len() == arity
            });
        let Some(first) = matching.next() else {
            let declares_one = context
                .index
                .own_members(context.this_item)
                .iter()
                .any(|&member| context.index.member(member).kind == DefKind::Constructor);
            // Arguments with no constructor to take them, or a constructor of every other arity: the
            // linter's to report, and nothing here can pick a descriptor for it.
            if declares_one || arity > 0 {
                return Err(LowerError::Unsupported(
                    "an `enum` constant with no matching constructor",
                ));
            }
            return Ok(None);
        };
        if matching.next().is_some() {
            return Err(LowerError::Unsupported(
                "an `enum` with two constructors of one arity",
            ));
        }
        Ok(Some(first))
    }

    /// `<clinit>`: every `static` field initialiser and `static { … }` block, in source order.
    ///
    /// `None` when the type has neither, because an empty `<clinit>` is still a method the JVM loads
    /// and calls. `ACC_STATIC` is required on it from major version 51 (JVMS §2.9.2), and it takes no
    /// access level at all — nothing can name it.
    fn class_initializer(
        context: &Context<'_>,
        pool: &mut ConstantPool,
        members: &[SyntaxNode],
        asserts: bool,
        constants: &[ast::EnumConstant],
        internal_name: &str,
    ) -> Result<Option<MethodInfo>> {
        if !asserts
            && constants.is_empty()
            && !members
                .iter()
                .any(|member| Self::initializes(member, context.in_interface, true))
        {
            return Ok(None);
        }
        let name_index = pool.utf8_index("<clinit>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index("()V").ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Static, "()V")?;
        let slots = Slots::new(context, None, true);
        let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, false);
        if asserts {
            Self::assertion_flag(context, &mut emit)?;
        }
        // An `enum`'s constants are built *first*, because a `static` initialiser below them may read
        // one and JLS §12.4.2 runs them in that order.
        if !constants.is_empty() {
            Self::enum_constants(constants, internal_name, context, &mut emit)?;
        }
        Self::initializers(context, &mut emit, members, true)?;
        if asm.reachable() {
            asm.return_(None)?;
        }
        Ok(Some(MethodInfo {
            access_flags: MethodAccessFlags(MethodAccessFlags::STATIC),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        }))
    }

    /// Build every `enum` constant, then the `$VALUES` array holding them in declaration order.
    ///
    /// The ordinal *is* the declaration position: it is what `ordinal()` returns, what a `switch` over
    /// the type indexes on, and what `compareTo` orders by. Numbering them any other way would be a
    /// class that verifies and compares wrongly.
    fn enum_constants(
        constants: &[ast::EnumConstant],
        internal_name: &str,
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
    ) -> Result<()> {
        let descriptor = alloc::format!("L{internal_name};");
        for (ordinal, constant) in constants.iter().enumerate() {
            let name = constant
                .name()
                .ok_or(LowerError::Unsupported("an `enum` constant with no name"))?;
            let arguments: Vec<ast::Expr> = constant
                .args()
                .into_iter()
                .flat_map(|list| list.args())
                .collect();
            let selected = Self::enum_constructor(arguments.len(), context)?;
            emit.asm.new_object(internal_name)?;
            emit.asm.dup()?;
            emit.asm.const_string(&name)?;
            emit.asm
                .const_int(i32::try_from(ordinal).map_err(|_| {
                    LowerError::Unsupported("an `enum` with this many constants")
                })?)?;
            // The name and the ordinal are already on the stack, so the written arguments follow them
            // exactly as the emitted descriptor names them.
            let init = match selected {
                Some(member) => {
                    let params = context.index.resolved_param_tys(member);
                    let varargs = context.index.member(member).varargs;
                    expr::Expr::arguments(&arguments, &params, varargs, context, emit)?;
                    let mut written =
                        Descriptor::method_descriptor(member, context.index, true)?.to_string();
                    written = alloc::format!("(L{STRING};I{}", written.trim_start_matches('('));
                    written
                }
                None => ENUM_INIT.to_owned(),
            };
            emit.asm
                .invoke_special(internal_name, "<init>", &init, false)?;
            emit.asm.put_static(internal_name, &name, &descriptor)?;
        }

        emit.asm.const_int(
            i32::try_from(constants.len())
                .map_err(|_| LowerError::Unsupported("an `enum` with this many constants"))?,
        )?;
        emit.asm.new_array(&descriptor)?;
        for (ordinal, constant) in constants.iter().enumerate() {
            let name = constant
                .name()
                .ok_or(LowerError::Unsupported("an `enum` constant with no name"))?;
            emit.asm.dup()?;
            emit.asm
                .const_int(i32::try_from(ordinal).map_err(|_| {
                    LowerError::Unsupported("an `enum` with this many constants")
                })?)?;
            emit.asm.get_static(internal_name, &name, &descriptor)?;
            emit.asm.array_store(&descriptor)?;
        }
        Ok(emit
            .asm
            .put_static(internal_name, VALUES, &alloc::format!("[{descriptor}"))?)
    }

    /// `$assertionsDisabled = !Foo.class.desiredAssertionStatus();`
    ///
    /// The first thing `<clinit>` does in a class containing an `assert`. The *negation* is what makes
    /// the guard one branch at each assertion site rather than two — the field is read and the
    /// assertion skipped when it is true.
    fn assertion_flag(context: &Context<'_>, emit: &mut Emit<'_, '_>) -> Result<()> {
        let enabled = emit.asm.label();
        let store = emit.asm.label();
        emit.asm.const_class(&context.this_class)?;
        emit.asm
            .invoke_virtual("java/lang/Class", "desiredAssertionStatus", "()Z")?;
        emit.asm.branch(
            crate::jvm::Branch::IntZero(crate::jvm::Compare::Ne),
            enabled,
        )?;
        emit.asm.const_int(1)?;
        emit.asm.branch(crate::jvm::Branch::Always, store)?;
        emit.asm.bind(enabled)?;
        emit.asm.const_int(0)?;
        emit.asm.bind(store)?;
        Ok(emit
            .asm
            .put_static(&context.this_class, stmt::ASSERTIONS_DISABLED, "Z")?)
    }

    /// Whether `member` contributes to the class initialiser (`statics`) or to every constructor.
    fn initializes(member: &SyntaxNode, in_interface: bool, statics: bool) -> bool {
        use jals_syntax::SyntaxKind::{INITIALIZER, STATIC_KW};
        match member.kind() {
            // An interface field is implicitly `static` (JLS §9.3), so it is written without the
            // keyword and still runs in `<clinit>`.
            FIELD_DECL => {
                (Self::has_modifier(member, STATIC_KW) || in_interface) == statics
                    && ast::FieldDecl::cast(member.clone())
                        .is_some_and(|decl| decl.value().is_some())
            }
            INITIALIZER => Self::has_modifier(member, STATIC_KW) == statics,
            _ => false,
        }
    }

    /// Emit the field initialisers and initialiser blocks that run in one of the two places they can.
    ///
    /// One walk in source order, because JLS §12.4.2 / §12.5 run them in the order they are written
    /// and a later one may read what an earlier one assigned.
    fn initializers(
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
        members: &[SyntaxNode],
        statics: bool,
    ) -> Result<()> {
        use jals_syntax::SyntaxKind::INITIALIZER;
        for member in members {
            if !Self::initializes(member, context.in_interface, statics) {
                continue;
            }
            if member.kind() == INITIALIZER {
                let block = ast::Initializer::cast(member.clone())
                    .and_then(|initializer| initializer.block());
                if let Some(block) = block {
                    stmt::Stmt::block(&block, context, emit)?;
                }
                continue;
            }
            let Some(decl) = ast::FieldDecl::cast(member.clone()) else {
                continue;
            };
            // The CST is flat, like a local declaration's: `int a = 1, b = 2;` is one declaration
            // whose two names take the two expression siblings in order. `value()` returns only the
            // first, which gave `b` the value of `a`.
            let values: Vec<_> = decl
                .syntax()
                .children()
                .filter_map(ast::Expr::cast)
                .collect();
            for (index, name) in decl.names().enumerate() {
                let Some(value) = values.get(index) else {
                    continue;
                };
                let field = context.member_at(&name)?;
                let ty = context.index.resolved_member_ty(field);
                let descriptor = Descriptor::descriptor_of(&ty, context.index)?.to_string();
                if !statics {
                    emit.asm.load(0)?;
                }
                // Converted to the field's declared type, which is where `long total = 0;` gets its
                // `i2l`.
                expr::Expr::lower_as(value, &ty, context, emit)?;
                if statics {
                    emit.asm
                        .put_static(&context.this_class, name.text(), &descriptor)?;
                } else {
                    emit.asm
                        .put_field(&context.this_class, name.text(), &descriptor)?;
                }
            }
        }
        Ok(())
    }

    /// The body's explicit constructor invocation — a bare `this(…)` or `super(…)`.
    ///
    /// JLS §8.8.7 puts it first or nowhere, so only the first statement is examined. Only the bare
    /// forms count: `this.method()` and `super.method()` are qualified calls whose callee is a field
    /// access rather than a name reference.
    fn explicit_constructor_invocation(body: &ast::Block) -> Option<ast::CallExpr> {
        use jals_syntax::SyntaxKind::{SUPER_KW, THIS_KW};
        let ast::Stmt::Expr(first) = body.stmts().next()? else {
            return None;
        };
        let ast::Expr::Call(call) = first.expr()? else {
            return None;
        };
        let ast::Expr::NameRef(name) = call.callee()? else {
            return None;
        };
        name.syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .any(|token| matches!(token.kind(), THIS_KW | SUPER_KW))
            .then_some(call)
    }

    /// Whether an explicit constructor invocation is the `this(…)` form rather than `super(…)`.
    fn delegates_to_this(call: &ast::CallExpr) -> bool {
        matches!(call.callee(), Some(ast::Expr::NameRef(name))
            if name
                .syntax()
                .children_with_tokens()
                .filter_map(jals_syntax::SyntaxElement::into_token)
                .any(|token| token.kind() == jals_syntax::SyntaxKind::THIS_KW))
    }

    /// `super()` followed by every instance field's initialiser — what a constructor runs before
    /// its own body, and the reason a field initialiser is not a statement anywhere in the source.
    fn prologue(
        context: &Context<'_>,
        emit: &mut Emit<'_, '_>,
        super_name: &str,
        super_item: Option<ItemId>,
        members: &[SyntaxNode],
        forwarded: Option<&MethodDescriptor>,
    ) -> Result<()> {
        // Three ways to reach the superclass constructor, and only the first is the plain `super()`.
        if let Some(descriptor) = forwarded {
            // An anonymous class's superclass constructor takes arguments the `new` handed over, and
            // this constructor's own parameters *are* those arguments, in order and at their own widths.
            emit.asm.load(0)?;
            let mut slot = 1 + u16::from(context.encloses.is_some());
            for param in &descriptor.params {
                emit.asm.load(slot)?;
                slot += Slots::descriptor_width(&param.to_string());
            }
            emit.asm.invoke_special(
                super_name,
                "<init>",
                &MethodDescriptor::to_string(descriptor),
                false,
            )?;
        } else if context.in_enum {
            // An `enum`'s superclass is `Enum`, which has no no-argument constructor: the implicit
            // delegation passes the two synthetic parameters straight through, which is the only way
            // the constant's name and ordinal ever reach it.
            emit.asm.load(0)?;
            emit.asm.load(1)?;
            emit.asm.load(2)?;
            emit.asm.invoke_special(ENUM, "<init>", ENUM_INIT, false)?;
        } else {
            // `super()` exists only if the superclass declares no constructor at all or declares a
            // no-argument one. Emitting the call regardless produces a class that loads and then
            // throws `NoSuchMethodError` at the first `new` — a run-time failure the compiler is in a
            // position to report instead.
            if let Some(super_item) = super_item {
                let mut constructors = context
                    .index
                    .own_members(super_item)
                    .iter()
                    .map(|&member| context.index.member(member))
                    .filter(|member| member.kind == DefKind::Constructor)
                    .peekable();
                if constructors.peek().is_some() && !constructors.any(|m| m.params.is_empty()) {
                    return Err(LowerError::Unsupported(
                        "a superclass with no no-argument constructor",
                    ));
                }
            }
            emit.asm.load(0)?;
            emit.asm
                .invoke_special(super_name, "<init>", "()V", false)?;
        }
        // The enclosing instance is stored after `super()` — before it, `this` is still
        // `UninitializedThis` and a `putfield` on it is not something the verifier accepts here. Before
        // the field initialisers, so one of them can already read it.
        if let Some(enclosing) = &context.encloses {
            emit.asm.load(0)?;
            emit.asm.load(1)?;
            emit.asm
                .put_field(&context.this_class, OUTER, &alloc::format!("L{enclosing};"))?;
        }
        // Each capture's parameter sits after every declared one, and the widths are what say where.
        let mut slot = emit.slots.next_free();
        for &captured in context.captured_here() {
            let name = Self::capture_field(captured, context);
            let descriptor = Self::capture_descriptor(captured, context)?;
            emit.asm.load(0)?;
            emit.asm.load(slot)?;
            emit.asm
                .put_field(&context.this_class, &name, &descriptor)?;
            slot += Slots::descriptor_width(&descriptor);
        }
        Self::initializers(context, emit, members, false)
    }
}

impl Context<'_> {
    /// What the lambda at `span` was compiled into, when this class has one there.
    pub(crate) fn lambda_at(&self, span: &core::ops::Range<usize>) -> Option<&Lambda> {
        self.lambdas.get(&(span.start, span.end))
    }

    /// The locals a local class in this file captures.
    pub(crate) fn captures_of_item(
        &self,
        item: ItemId,
    ) -> Option<alloc::vec::Vec<jals_hir::DefId>> {
        self.captures.get(&item).cloned()
    }

    /// Whether `id` is a local the class being compiled captures.
    pub(crate) fn captures_local(&self, id: jals_hir::DefId) -> bool {
        self.captured_here().contains(&id)
    }

    /// The locals the class being compiled captures.
    fn captured_here(&self) -> &[jals_hir::DefId] {
        self.captures
            .get(&self.this_item)
            .map_or(&[], alloc::vec::Vec::as_slice)
    }

    /// The indexed member the name token `token` declares.
    fn member_at(&self, token: &SyntaxToken) -> Result<jals_hir::MemberId> {
        self.index
            .member_by_decl(self.file, usize::from(token.text_range().start()))
            .ok_or_else(|| LowerError::Unresolved(token.text().into()))
    }

    /// The type a `TYPE` node names.
    ///
    /// Inference keys its record by *expression* span and a `TYPE` node is not an expression, so an
    /// `instanceof`'s target has nowhere to be read from and is resolved here instead. Only what a
    /// `Class` entry needs is recovered — the array dimensions and the item the name binds to — and a
    /// name the index does not hold is reported rather than guessed at, because an invented package
    /// produces a class that loads and then throws `NoClassDefFoundError`.
    fn ty_of_type(&self, node: &ast::Type) -> Result<jals_hir::Ty> {
        use jals_syntax::SyntaxKind::LBRACK;
        let dimensions = node
            .syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| token.kind() == LBRACK)
            .count();

        let mut ty = if node.is_primitive_or_var() {
            jals_hir::Ty::Primitive(Self::primitive_of(node).ok_or(DescError::Unknown)?)
        } else {
            let name = node.simple_name().ok_or(DescError::Unknown)?;
            let qualified = node.is_qualified().then(|| node.qualified_text()).flatten();
            let id = self
                .index
                .resolve_type_name(self.file, &name, qualified.as_deref())
                .project_id()
                .ok_or_else(|| DescError::Unresolved(name.clone()))?;
            jals_hir::Ty::Class(jals_hir::ClassTy::Project {
                id,
                name,
                args: Vec::new(),
            })
        };
        for _ in 0..dimensions {
            ty = jals_hir::Ty::Array(alloc::boxed::Box::new(ty));
        }
        Ok(ty)
    }

    /// The nearest type every entry in `types` is assignable to.
    ///
    /// What a multi-catch's binding has. Walked over the *class* chain only, because a common
    /// interface would not be a `catch` type; a set with no common ancestor the index holds falls back
    /// to `Throwable`, which every catchable type is one of.
    fn common_supertype(&self, types: &[jals_hir::Ty]) -> jals_hir::Ty {
        let throwable = || jals_hir::Ty::Class(jals_hir::ClassTy::external("java.lang.Throwable"));
        let ids: Option<Vec<ItemId>> = types
            .iter()
            .map(|ty| match ty {
                jals_hir::Ty::Class(jals_hir::ClassTy::Project { id, .. }) => Some(*id),
                _ => None,
            })
            .collect();
        let Some(ids) = ids else { return throwable() };
        let Some((&first, rest)) = ids.split_first() else {
            return throwable();
        };
        let mut candidate = first;
        loop {
            if rest
                .iter()
                .all(|&other| self.index.is_subtype(other, candidate))
            {
                let fqn = self.index.item(candidate).fqn.as_str();
                return jals_hir::Ty::Class(jals_hir::ClassTy::Project {
                    id: candidate,
                    name: fqn.rsplit('.').next().unwrap_or(fqn).to_owned(),
                    args: Vec::new(),
                });
            }
            let Some(next) = self
                .index
                .item(candidate)
                .supertypes
                .iter()
                .map(|supertype| supertype.id)
                .find(|&id| self.index.item(id).kind != DefKind::Interface)
            else {
                return throwable();
            };
            candidate = next;
        }
    }

    /// The type a *name* names, when the grammar parsed it as an expression.
    ///
    /// `String.class`'s base is a name reference, not a type node, because nothing tells the parser
    /// which of the two it is until the `.class` arrives. So the dotted text is resolved against the
    /// index directly.
    fn ty_of_name(&self, node: &SyntaxNode) -> Result<jals_hir::Ty> {
        let text: String = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .filter(|token| {
                matches!(
                    token.kind(),
                    jals_syntax::SyntaxKind::IDENT | jals_syntax::SyntaxKind::DOT
                )
            })
            .map(|token| token.text().to_owned())
            .collect();
        let simple = text.rsplit('.').next().unwrap_or(&text).to_owned();
        let qualified = text.contains('.').then(|| text.clone());
        let id = self
            .index
            .resolve_type_name(self.file, &simple, qualified.as_deref())
            .project_id()
            .ok_or_else(|| DescError::Unresolved(simple.clone()))?;
        Ok(jals_hir::Ty::Class(jals_hir::ClassTy::Project {
            id,
            name: simple,
            args: Vec::new(),
        }))
    }

    /// The primitive a `TYPE` node's keyword names.
    fn primitive_of(node: &ast::Type) -> Option<jals_hir::Primitive> {
        use jals_hir::Primitive;
        use jals_syntax::SyntaxKind::{
            BOOLEAN_KW, BYTE_KW, CHAR_KW, DOUBLE_KW, FLOAT_KW, INT_KW, LONG_KW, SHORT_KW,
        };
        node.syntax()
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find_map(|token| {
                Some(match token.kind() {
                    BOOLEAN_KW => Primitive::Boolean,
                    BYTE_KW => Primitive::Byte,
                    SHORT_KW => Primitive::Short,
                    CHAR_KW => Primitive::Char,
                    INT_KW => Primitive::Int,
                    LONG_KW => Primitive::Long,
                    FLOAT_KW => Primitive::Float,
                    DOUBLE_KW => Primitive::Double,
                    _ => return None,
                })
            })
    }

    /// A node's byte span, keyed the way the inference memo is: the node's own range, leading
    /// trivia included, because that is what the analysis recorded against.
    fn span(node: &SyntaxNode) -> core::ops::Range<usize> {
        let range = node.text_range();
        usize::from(range.start())..usize::from(range.end())
    }

    /// The definition a name-reference node binds to.
    ///
    /// Keyed by the identifier *token*, not the node: a `NAME_REF` carries its leading trivia and
    /// the resolver indexes references by where the name itself starts.
    fn def_at(&self, node: &SyntaxNode) -> Option<jals_hir::DefId> {
        let token = node
            .children_with_tokens()
            .filter_map(jals_syntax::SyntaxElement::into_token)
            .find(|token| token.kind() == jals_syntax::SyntaxKind::IDENT)?;
        let start = usize::from(token.text_range().start());
        self.resolved
            .reference_at(start)
            .and_then(|reference| reference.resolution.def_id())
            .or_else(|| self.resolved.symbol_at(start))
    }
}
