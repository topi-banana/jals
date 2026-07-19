//! Pass 1: walk the CST, creating scopes and registering definitions.
//!
//! Each `build_*` method handles one shape. The driver [`Resolver::build`] dispatches on node kind;
//! anything without a special shape just recurses its children in the current scope, which also
//! records the `NAME_REF`s it meets (deeper resolution happens in pass 2).

use jals_syntax::SyntaxKind::{
    ANNOTATION_TYPE_DECL, BLOCK, CATCH_CLAUSE, CLASS_BODY, CLASS_DECL, CONSTRUCTOR_DECL,
    ENUM_CONSTANT, ENUM_DECL, FIELD_DECL, FOR_EACH_STMT, FOR_STMT, INTERFACE_DECL, LAMBDA_EXPR,
    LOCAL_VAR_DECL, METHOD_DECL, NAME_REF, NEW_EXPR, PARAM, PARAM_LIST, RECORD_COMPONENT,
    RECORD_DECL, RECORD_HEADER, SWITCH_EXPR, SWITCH_GROUP, SWITCH_LABEL, SWITCH_RULE, SWITCH_STMT,
    TRY_STMT, TYPE, TYPE_PARAM, TYPE_PARAMS,
};
use jals_syntax::SyntaxNode;
use jals_syntax::ast::{
    AstNode, CatchClause, ForEachStmt, LambdaExpr, LocalVarDecl, Resource, SwitchExpr, SwitchStmt,
    TryStmt,
};
use jals_syntax::ast::{FieldDecl, ResourceList};

use super::Resolver;
use super::collect::Collect;
use crate::def::DefKind;
use crate::scope::{ScopeId, ScopeKind};

impl Resolver {
    /// Dispatches on `node`'s kind, building scopes/definitions and recording references in `scope`.
    ///
    /// The one boxed shim of the pass-1 recursion: every `build_*` helper recurses back through
    /// here, so the async cycle has a single `Box::pin` choke point.
    pub(super) fn build<'a>(
        &'a mut self,
        node: &'a SyntaxNode,
        scope: ScopeId,
    ) -> jals_exec::LocalBoxFuture<'a, ()> {
        alloc::boxed::Box::pin(self.build_impl(node, scope))
    }

    /// The per-node dispatch behind [`Resolver::build`].
    async fn build_impl(&mut self, node: &SyntaxNode, scope: ScopeId) {
        self.tick().await;
        match node.kind() {
            CLASS_DECL | INTERFACE_DECL | ENUM_DECL | RECORD_DECL | ANNOTATION_TYPE_DECL => {
                self.build_type_decl(node, scope).await;
            }
            METHOD_DECL | CONSTRUCTOR_DECL => self.build_method(node, scope).await,
            FIELD_DECL => {
                if let Some(field) = FieldDecl::cast(node.clone()) {
                    for tok in field.names() {
                        self.add_def(scope, DefKind::Field, &tok);
                    }
                }
                self.build_children(node, scope).await;
            }
            LOCAL_VAR_DECL => {
                if let Some(local) = LocalVarDecl::cast(node.clone()) {
                    for tok in local.names() {
                        self.add_def(scope, DefKind::Local, &tok);
                    }
                }
                self.build_children(node, scope).await;
            }
            ENUM_CONSTANT => self.build_enum_constant(node, scope).await,
            BLOCK => {
                let bs = self.new_scope(ScopeKind::Block, scope, node);
                self.build_children(node, bs).await;
            }
            FOR_STMT => {
                let fs = self.new_scope(ScopeKind::For, scope, node);
                self.build_children(node, fs).await;
            }
            FOR_EACH_STMT => self.build_for_each(node, scope).await,
            TRY_STMT => self.build_try(node, scope).await,
            CATCH_CLAUSE => self.build_catch(node, scope).await,
            SWITCH_STMT | SWITCH_EXPR => self.build_switch(node, scope).await,
            LAMBDA_EXPR => self.build_lambda(node, scope).await,
            NEW_EXPR => {
                // An anonymous-class body gets its own type scope; the qualifier/args stay in the
                // enclosing scope.
                for child in node.children() {
                    if child.kind() == CLASS_BODY {
                        self.build_anon_type(&child, scope).await;
                    } else {
                        self.build(&child, scope).await;
                    }
                }
            }
            NAME_REF => self.record_ref(scope, node),
            TYPE => {
                // A type-name occurrence is a Type-namespace reference. Recurse so that nested type
                // arguments (`List<Foo>` — the inner `Foo`) are recorded as their own references.
                self.record_type_ref(scope, node);
                self.build_children(node, scope).await;
            }
            _ => self.build_children(node, scope).await,
        }
    }

    /// Recurses every child node of `node` in `scope`.
    async fn build_children(&mut self, node: &SyntaxNode, scope: ScopeId) {
        for child in node.children() {
            self.build(&child, scope).await;
        }
    }

    async fn build_type_decl(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let kind = match node.kind() {
            CLASS_DECL => DefKind::Class,
            INTERFACE_DECL => DefKind::Interface,
            ENUM_DECL => DefKind::Enum,
            RECORD_DECL => DefKind::Record,
            _ => DefKind::AnnotationType,
        };
        // The type's own name lives in the *enclosing* scope, visible to its siblings.
        if let Some(tok) = Collect::first_ident_token(node) {
            self.add_def(scope, kind, &tok);
        }
        let ts = self.new_scope(ScopeKind::Type, scope, node);
        self.register_type_params(node, ts);
        // Record components are value bindings (effectively fields) of the record body.
        if let Some(header) = node.children().find(|c| c.kind() == RECORD_HEADER) {
            for comp in header.children().filter(|c| c.kind() == RECORD_COMPONENT) {
                if let Some(tok) = Collect::first_ident_token(&comp) {
                    self.add_def(ts, DefKind::Field, &tok);
                }
            }
        }
        self.build_children(node, ts).await;
    }

    async fn build_method(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let kind = if node.kind() == CONSTRUCTOR_DECL {
            DefKind::Constructor
        } else {
            DefKind::Method
        };
        if let Some(tok) = Collect::first_ident_token(node) {
            self.add_def(scope, kind, &tok);
        }
        let ms = self.new_scope(ScopeKind::Method, scope, node);
        self.register_type_params(node, ms);
        // Only a body-bearing executable's parameters are registered: an abstract / interface
        // parameter can never be referenced, so omitting it keeps it out of unused diagnostics.
        let has_body = node.children().any(|c| c.kind() == BLOCK);
        if has_body && let Some(plist) = node.children().find(|c| c.kind() == PARAM_LIST) {
            for p in plist.children().filter(|c| c.kind() == PARAM) {
                if let Some(tok) = Collect::first_ident_token(&p) {
                    self.add_def(ms, DefKind::Param, &tok);
                }
            }
        }
        self.build_children(node, ms).await;
    }

    fn register_type_params(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(tps) = node.children().find(|c| c.kind() == TYPE_PARAMS) {
            for tp in tps.children().filter(|c| c.kind() == TYPE_PARAM) {
                if let Some(tok) = Collect::first_ident_token(&tp) {
                    self.add_def(scope, DefKind::TypeParam, &tok);
                }
            }
        }
    }

    async fn build_enum_constant(&mut self, node: &SyntaxNode, scope: ScopeId) {
        if let Some(tok) = Collect::first_ident_token(node) {
            self.add_def(scope, DefKind::EnumConstant, &tok);
        }
        for child in node.children() {
            if child.kind() == CLASS_BODY {
                self.build_anon_type(&child, scope).await;
            } else {
                self.build(&child, scope).await;
            }
        }
    }

    /// Builds an anonymous-class / enum-constant body as its own type scope.
    async fn build_anon_type(&mut self, body: &SyntaxNode, scope: ScopeId) {
        let ts = self.new_scope(ScopeKind::Type, scope, body);
        self.build_children(body, ts).await;
    }

    async fn build_for_each(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let fs = self.new_scope(ScopeKind::For, scope, node);
        let Some(fe) = ForEachStmt::cast(node.clone()) else {
            return;
        };
        if let Some(tok) = Collect::first_ident_token(node) {
            self.add_def(fs, DefKind::Local, &tok);
        }
        // The element type is a type reference (`for (Foo f : ...)`); it does not see the variable.
        if let Some(ty) = fe.ty() {
            self.build(ty.syntax(), fs).await;
        }
        // The iterable is evaluated where the loop sits — the loop variable is not visible to it.
        if let Some(it) = fe.iterable() {
            self.build(it.syntax(), scope).await;
        }
        if let Some(body) = fe.body() {
            self.build(body.syntax(), fs).await;
        }
    }

    async fn build_try(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let Some(t) = TryStmt::cast(node.clone()) else {
            self.build_children(node, scope).await;
            return;
        };
        // Resources are visible in the try block; catch/finally do not see them.
        let body_scope = match t.resources() {
            Some(res) => self.build_resources(&res, scope).await,
            None => scope,
        };
        if let Some(b) = t.block() {
            self.build(b.syntax(), body_scope).await;
        }
        for c in t.catches() {
            self.build(c.syntax(), scope).await;
        }
        if let Some(f) = t.finally() {
            self.build(f.syntax(), scope).await;
        }
    }

    async fn build_resources(&mut self, res: &ResourceList, scope: ScopeId) -> ScopeId {
        let rs = self.new_scope(ScopeKind::Resources, scope, res.syntax());
        for r in res.resources() {
            if let Some(tok) = Resource::cast(r.syntax().clone()).and_then(|r| r.binding()) {
                self.add_def(rs, DefKind::Resource, &tok);
            }
            // A resource initializer can reference resources declared before it (sequential).
            self.build_children(r.syntax(), rs).await;
        }
        rs
    }

    async fn build_catch(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let cs = self.new_scope(ScopeKind::Catch, scope, node);
        let Some(catch) = CatchClause::cast(node.clone()) else {
            return;
        };
        if let Some(tok) = catch.binding() {
            self.add_def(cs, DefKind::CatchParam, &tok);
        }
        // Recurse every child node (the caught type(s) and the block) in the catch scope; the
        // binding is a bare token, not a node, so it is not revisited here. This records the
        // exception type(s) (`catch (IOException e)`) as type references.
        for child in node.children() {
            self.build(&child, cs).await;
        }
    }

    async fn build_switch(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let (selector, body) = if node.kind() == SWITCH_STMT {
            SwitchStmt::cast(node.clone()).map_or((None, None), |s| (s.selector(), s.body()))
        } else {
            SwitchExpr::cast(node.clone()).map_or((None, None), |s| (s.selector(), s.body()))
        };
        if let Some(sel) = selector {
            self.build(sel.syntax(), scope).await;
        }
        let Some(body) = body else {
            return;
        };
        for child in body.syntax().children() {
            match child.kind() {
                SWITCH_RULE | SWITCH_GROUP => {
                    let ss = self.new_scope(ScopeKind::Switch, scope, &child);
                    for label in child.children().filter(|c| c.kind() == SWITCH_LABEL) {
                        for tok in Collect::pattern_var_tokens(&label) {
                            self.add_def(ss, DefKind::PatternVar, &tok);
                        }
                    }
                    // Guard and body resolve in the switch scope, seeing the pattern variables.
                    self.build_children(&child, ss).await;
                }
                _ => self.build(&child, scope).await,
            }
        }
    }

    async fn build_lambda(&mut self, node: &SyntaxNode, scope: ScopeId) {
        let ls = self.new_scope(ScopeKind::Lambda, scope, node);
        let Some(lambda) = LambdaExpr::cast(node.clone()) else {
            return;
        };
        if let Some(params) = lambda.params() {
            for p in params.params() {
                if let Some(tok) = Collect::first_ident_token(p.syntax()) {
                    self.add_def(ls, DefKind::LambdaParam, &tok);
                }
                // An explicitly-typed parameter (`(Foo f) -> ...`) contributes a type reference.
                for ty in p.syntax().children().filter(|c| c.kind() == TYPE) {
                    self.build(&ty, ls).await;
                }
            }
        }
        if let Some(b) = lambda.block_body() {
            self.build(b.syntax(), ls).await;
        }
        if let Some(e) = lambda.expr_body() {
            self.build(e.syntax(), ls).await;
        }
    }
}
