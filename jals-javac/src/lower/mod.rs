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
//! A `static` nested type is its own class file, named `Outer$Inner` and listed in an `InnerClasses`
//! attribute — which is the only place a nested type's `private` and `static` can live. Referring to one
//! *by name* still needs resolution the index does not do: a simple or partly-qualified nested name
//! (`Counter`, `Outer.Counter`) resolves against packages and imports only, so the reference reports
//! even where the declaration compiles.
//!
//! Not yet at all: varargs, `Signature` attributes and bridge methods, lambdas, method references,
//! non-`static` inner classes, local and anonymous classes, and `enum` / `record` declarations. Each
//! arrives with the milestone that can test it.

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
    MethodDescriptor, MethodInfo,
};
use jals_hir::{DefKind, FileId, ItemId, ProjectIndex, Resolved, TypeInference};
use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_DECL, FIELD_DECL, INTERFACE_DECL,
    METHOD_DECL, RECORD_DECL,
};
use jals_syntax::ast::{self, AstNode as _};
use jals_syntax::{SyntaxNode, SyntaxToken};

use crate::desc::{DescError, Descriptor};
use crate::jvm::{AsmError, Assembler, Receiver};
use crate::lower::slots::Slots;

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
            if !matches!(node.kind(), CLASS_DECL | INTERFACE_DECL) {
                continue;
            }
            // A *local* or anonymous class is nested inside a method body rather than a class body,
            // and its captured locals need a synthetic constructor parameter each. `Stmt::block`
            // reports it where it appears, and skipping it here keeps that the report.
            if node
                .ancestors()
                .skip(1)
                .any(|ancestor| ancestor.kind() == jals_syntax::SyntaxKind::BLOCK)
            {
                continue;
            }
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
        if Self::is_nested(node)
            && !Self::has_modifier(node, jals_syntax::SyntaxKind::STATIC_KW)
            && index.item(item).kind != DefKind::Interface
        {
            return Err(LowerError::Unsupported("a non-`static` inner class"));
        }
        let internal_name = Descriptor::internal_name_of(item, index);
        let is_interface = index.item(item).kind == DefKind::Interface;
        let context = Context {
            index,
            inference,
            resolved,
            file,
            this_class: internal_name.clone(),
            in_interface: is_interface,
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
        let super_name = super_item.map_or_else(
            || "java/lang/Object".to_owned(),
            |id| Descriptor::internal_name_of(id, index),
        );
        let super_class = pool.class_index(&super_name).ok_or(AsmError::PoolFull)?;

        let body = node
            .children()
            .find(|child| ast::ClassBody::cast(child.clone()).is_some());
        let members: Vec<SyntaxNode> = body
            .map(|body| body.children().collect())
            .unwrap_or_default();

        let mut fields = Vec::new();
        let mut methods = Vec::new();
        let mut saw_constructor = false;

        for member in &members {
            // A nested type is its own class file, compiled by `file` rather than here. An `enum`, a
            // `record`, and an `@interface` are not compiled at all yet, and dropping one silently
            // would produce a class that loads and then throws `NoClassDefFoundError` at the first use
            // — the exact failure a compiler that reports nothing has to avoid.
            match member.kind() {
                CLASS_DECL | INTERFACE_DECL => continue,
                ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => {
                    return Err(LowerError::Unsupported("a nested `enum` or `record`"));
                }
                _ => {}
            }
            // A non-`static` nested class holds a reference to its enclosing instance, which means a
            // synthetic field *and* an extra parameter on every constructor — so the descriptors the
            // index computed from the declaration would all be one parameter short.
            match member.kind() {
                FIELD_DECL => Self::field(member, &context, &mut pool, &mut fields)?,
                METHOD_DECL => methods.push(Self::method(member, &context, &mut pool)?),
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
        if !saw_constructor && !is_interface {
            methods.push(Self::default_constructor(
                &context,
                &mut pool,
                &super_name,
                super_item,
                &members,
                Self::access_level(node),
            )?);
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
        // A `static` field's initialiser and a `static { … }` block both run in `<clinit>`, once,
        // when the class is first used. Nothing else runs them — so dropping them produced a class
        // whose `static int n = 5;` read back as 0, which is a silent miscompile rather than a
        // missing feature.
        if let Some(class_init) = Self::class_initializer(&context, &mut pool, &members, asserts)? {
            methods.push(class_init);
        }

        let nesting = Self::inner_classes(node, &context, &mut pool)?;

        let mut class = ClassFile::new(class_version, 0, pool);
        class.access_flags = ClassAccessFlags(Self::class_flags(node, is_interface));
        class.this_class = this_class;
        class.super_class = super_class;
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
            .filter(|child| matches!(child.kind(), CLASS_DECL | INTERFACE_DECL))
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
                .find(|ancestor| matches!(ancestor.kind(), CLASS_DECL | INTERFACE_DECL))
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
            let is_interface = context.index.item(item).kind == DefKind::Interface;
            let mut flags = Self::class_flags(declaration, is_interface) & !ClassAccessFlags::SUPER;
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

    fn class_flags(node: &SyntaxNode, is_interface: bool) -> u16 {
        // Only `public` is expressible on a top-level type. `private` / `protected` are nested-type
        // modifiers, and a nested type is reported rather than emitted.
        let mut flags = Self::access_level(node) & ClassAccessFlags::PUBLIC;
        if is_interface {
            // An interface is implicitly abstract and never has the `super`-call semantics bit.
            flags |= ClassAccessFlags::INTERFACE | ClassAccessFlags::ABSTRACT;
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
        for name in decl.names() {
            let member = context.member_at(&name)?;
            let descriptor = Descriptor::field_descriptor(member, context.index)?.to_string();
            out.push(FieldInfo {
                access_flags: FieldAccessFlags(Self::field_flags(node, context.in_interface)),
                name_index: pool.utf8_index(name.text()).ok_or(AsmError::PoolFull)?,
                descriptor_index: pool.utf8_index(&descriptor).ok_or(AsmError::PoolFull)?,
                attributes: Vec::new(),
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
        let descriptor = Descriptor::method_descriptor(member, context.index, false)?;
        let is_static = context.index.member(member).modifiers.is_static;

        let flags = Self::method_flags(node, context.in_interface);
        let text = MethodDescriptor::to_string(&descriptor);
        let name_index = pool.utf8_index(&name).ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&text).ok_or(AsmError::PoolFull)?;
        let attributes = match decl.body() {
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
        // No body means no `Code` attribute, and the JVM accepts that only from a method whose flags
        // say why it has none. `native` says so with its own flag — already set above — and
        // `ACC_NATIVE | ACC_ABSTRACT` is a pair JVMS §4.6 forbids, which a JVM rejects with "illegal
        // modifiers: 0x500". `abstract` says so directly, and an interface method says so implicitly
        // (JLS §9.4). Anything else with no body is a declaration the JVM would refuse.
        let flags = if attributes.is_empty() && flags & MethodAccessFlags::NATIVE == 0 {
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
        let descriptor = Descriptor::method_descriptor(member, context.index, true)?;
        let text = MethodDescriptor::to_string(&descriptor);
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index(&text).ok_or(AsmError::PoolFull)?;

        let body = node.children().find_map(ast::Block::cast);
        let delegation = body
            .as_ref()
            .and_then(Self::explicit_constructor_invocation);

        let params = node.children().find_map(ast::ParamList::cast);
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), &text)?;
        let slots = Slots::new(context, params.as_ref(), false);
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
            None => Self::prologue(context, &mut emit, super_name, super_item, members)?,
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
    ) -> Result<MethodInfo> {
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index("()V").ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), "()V")?;
        let slots = Slots::new(context, None, false);
        let mut emit = Emit::new(&mut asm, slots, jals_hir::Ty::Void, true);
        Self::prologue(context, &mut emit, super_name, super_item, members)?;
        asm.return_(None)?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(access),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
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
    ) -> Result<Option<MethodInfo>> {
        if !asserts
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
    ) -> Result<()> {
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
        Self::initializers(context, emit, members, false)
    }
}

impl Context<'_> {
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
