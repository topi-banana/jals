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
//! Classes with fields and methods, local variables, literals, arithmetic and comparison, `if` /
//! `while`, assignment, and calls (`static`, virtual, and interface). Constructors run `super()`
//! and their field initialisers. Not yet: `new`, arrays, `switch`, exceptions, generics beyond
//! erasure, lambdas, and inner classes — each arrives with the milestone that can test it.

mod expr;
mod slots;
mod stmt;

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
        for node in root.children() {
            if !matches!(node.kind(), CLASS_DECL | INTERFACE_DECL) {
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
        let internal_name = Descriptor::internal_name(index.item(item).fqn.as_str());
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
        let super_name = index
            .item(item)
            .supertypes
            .iter()
            .map(|supertype| index.item(supertype.id))
            .find(|super_item| super_item.kind != DefKind::Interface)
            .map_or_else(
                || "java/lang/Object".to_owned(),
                |super_item| Descriptor::internal_name(super_item.fqn.as_str()),
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
            // A nested type is its own class file, which this milestone does not emit. Dropping it
            // silently would produce a class that loads and then throws `NoClassDefFoundError` at
            // the first use — the exact failure mode a compiler that reports nothing has to avoid.
            if matches!(
                member.kind(),
                CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL
            ) {
                return Err(LowerError::Unsupported("a nested type declaration"));
            }
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
                &members,
                Self::access_level(node),
            )?);
        }

        let mut class = ClassFile::new(class_version, 0, pool);
        class.access_flags = ClassAccessFlags(Self::class_flags(node, is_interface));
        class.this_class = this_class;
        class.super_class = super_class;
        class.fields = fields;
        class.methods = methods;
        Ok(CompiledClass {
            internal_name,
            bytes: class.write(),
        })
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
    fn method_flags(node: &SyntaxNode, in_interface: bool) -> u16 {
        use jals_syntax::SyntaxKind::{
            ABSTRACT_KW, FINAL_KW, NATIVE_KW, STATIC_KW, STRICTFP_KW, SYNCHRONIZED_KW,
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
            (NATIVE_KW, MethodAccessFlags::NATIVE),
            (SYNCHRONIZED_KW, MethodAccessFlags::SYNCHRONIZED),
            (STRICTFP_KW, MethodAccessFlags::STRICT),
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
                let mut slots = Slots::new(context, decl.params().as_ref(), is_static);
                stmt::Stmt::block(&body, context, &mut asm, &mut slots)?;
                // A `void` body may simply run off its end; the JVM needs the instruction anyway.
                if matches!(descriptor.return_type, jals_classfile::ReturnType::Void) {
                    let _ = asm.return_(None);
                }
                alloc::vec![asm.finish()?]
            }
            // An abstract or interface method has no `Code` attribute at all.
            None => Vec::new(),
        };
        // No body means no `Code` attribute, which the JVM only accepts for a method that declares
        // it has none. A `static` method must always have one, so it is left alone and the class
        // is rejected at load time rather than silently made abstract.
        let flags = if attributes.is_empty() && !is_static {
            flags | MethodAccessFlags::ABSTRACT
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

        let params = node.children().find_map(ast::ParamList::cast);
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), &text)?;
        let mut slots = Slots::new(context, params.as_ref(), false);
        Self::prologue(context, &mut asm, &slots, super_name, members)?;
        if let Some(body) = node.children().find_map(ast::Block::cast) {
            stmt::Stmt::block(&body, context, &mut asm, &mut slots)?;
        }
        let _ = asm.return_(None);

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
        members: &[SyntaxNode],
        access: u16,
    ) -> Result<MethodInfo> {
        let name_index = pool.utf8_index("<init>").ok_or(AsmError::PoolFull)?;
        let descriptor_index = pool.utf8_index("()V").ok_or(AsmError::PoolFull)?;
        let mut asm = Assembler::new(pool, Receiver::Constructor(&context.this_class), "()V")?;
        let slots = Slots::new(context, None, false);
        Self::prologue(context, &mut asm, &slots, super_name, members)?;
        asm.return_(None)?;
        Ok(MethodInfo {
            access_flags: MethodAccessFlags(access),
            name_index,
            descriptor_index,
            attributes: alloc::vec![asm.finish()?],
        })
    }

    /// `super()` followed by every instance field's initialiser — what a constructor runs before
    /// its own body, and the reason a field initialiser is not a statement anywhere in the source.
    fn prologue(
        context: &Context<'_>,
        asm: &mut Assembler<'_>,
        slots: &Slots,
        super_name: &str,
        members: &[SyntaxNode],
    ) -> Result<()> {
        asm.load(0)?;
        asm.invoke_special(super_name, "<init>", "()V", false)?;

        for member in members {
            if member.kind() != FIELD_DECL {
                continue;
            }
            if Self::has_modifier(member, jals_syntax::SyntaxKind::STATIC_KW) {
                continue;
            }
            let Some(decl) = ast::FieldDecl::cast(member.clone()) else {
                continue;
            };
            let Some(value) = decl.value() else {
                continue;
            };
            for name in decl.names() {
                let field = context.member_at(&name)?;
                let descriptor = Descriptor::field_descriptor(field, context.index)?.to_string();
                asm.load(0)?;
                expr::Expr::lower(&value, context, asm, slots)?;
                asm.put_field(&context.this_class, name.text(), &descriptor)?;
            }
        }
        Ok(())
    }
}

impl Context<'_> {
    /// The indexed member the name token `token` declares.
    fn member_at(&self, token: &SyntaxToken) -> Result<jals_hir::MemberId> {
        self.index
            .member_by_decl(self.file, usize::from(token.text_range().start()))
            .ok_or_else(|| LowerError::Unresolved(token.text().into()))
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
