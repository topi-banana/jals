//! Protocol-neutral editor queries over one project index.

use alloc::borrow::ToOwned;
use alloc::string::String;
use alloc::vec::Vec;
use core::ops::Range;

use jals_hir::{
    DefKind, FileId, ItemId, ItemOrigin, Namespace, ProjectIndex, Resolution, Resolved, Ty,
    TypeResolution,
};
use jals_syntax::ast::{self, AstNode};
use jals_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

/// A parsed and file-locally resolved file supplied to a project query.
///
/// The CST handle is a cheap clone of rowan's immutable tree; the comparatively large resolution
/// result is borrowed so hosts can keep it in a lazy cache.
#[derive(Clone)]
pub struct QueryFile<'a> {
    /// Stable identity within the associated [`ProjectIndex`].
    pub file: FileId,
    /// The file's immutable syntax tree.
    pub syntax: SyntaxNode,
    /// File-local name resolution for `syntax`.
    pub resolved: &'a Resolved,
}

impl<'a> QueryFile<'a> {
    /// Bundle one file's analysis inputs.
    pub const fn new(file: FileId, syntax: SyntaxNode, resolved: &'a Resolved) -> Self {
        Self {
            file,
            syntax,
            resolved,
        }
    }
}

/// A byte range in a file. Adapters map the file id to a URI/path and the byte range to their
/// coordinate system.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileRange {
    pub file: FileId,
    pub range: Range<usize>,
}

/// Protocol-neutral completion categories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompletionKind {
    Method,
    Field,
    EnumMember,
    Variable,
    TypeParameter,
    Class,
    Interface,
    Enum,
    Keyword,
}

/// One completion candidate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Completion {
    pub label: String,
    pub kind: CompletionKind,
    pub detail: String,
}

/// Whether an occurrence reads or writes the highlighted binding.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HighlightKind {
    Read,
    Write,
}

/// One occurrence highlight in the current document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Highlight {
    pub range: Range<usize>,
    pub kind: HighlightKind,
}

/// Semantic editor queries shared by protocol adapters.
pub struct ProjectQueries<'a> {
    index: &'a ProjectIndex,
    current: QueryFile<'a>,
}

impl<'a> ProjectQueries<'a> {
    /// Create a query module over `index` and the current file.
    pub const fn new(index: &'a ProjectIndex, current: QueryFile<'a>) -> Self {
        Self { index, current }
    }

    /// Resolve a definition in file-local → project type → inferred member order.
    pub fn definition(&self, offset: usize) -> Option<FileRange> {
        if let Some((file, range)) =
            self.index
                .definition_at(self.current.file, self.current.resolved, offset)
        {
            return Some(FileRange { file, range });
        }
        let (file, range) = self.member_definition(offset)?;
        Some(FileRange { file, range })
    }

    /// Find references to the symbol at `offset`.
    ///
    /// `files` is consumed only for a project type. A local binding returns before touching it,
    /// allowing a host to supply lazily resolved project files.
    pub fn references<'b>(
        &self,
        offset: usize,
        include_declaration: bool,
        files: impl IntoIterator<Item = QueryFile<'b>>,
    ) -> Vec<FileRange> {
        let Some(ident) = self.ident_at(offset) else {
            return Vec::new();
        };
        let anchor = usize::from(ident.text_range().start());

        if let Some(def_id) = self.current.resolved.symbol_at(anchor) {
            if let Some(item) = self.index.item_by_decl(
                self.current.file,
                self.current.resolved.def(def_id).name_range.start,
            ) {
                return self.item_references(item, include_declaration, files);
            }
            return self
                .current
                .resolved
                .occurrences(def_id, include_declaration)
                .into_iter()
                .map(|range| FileRange {
                    file: self.current.file,
                    range,
                })
                .collect();
        }

        if let Some(item) = self.cross_file_type_at(anchor) {
            return self.item_references(item, include_declaration, files);
        }
        Vec::new()
    }

    /// The inferred type under `offset`, suppressing an uninformative unknown result.
    pub fn hover(&self, offset: usize) -> Option<Ty> {
        let inference = jals_hir::TypeInference::infer(
            &self.current.syntax,
            self.current.resolved,
            self.index,
            self.current.file,
        );
        let ty = inference.type_at(offset)?;
        (!matches!(ty, Ty::Unknown)).then(|| ty.clone())
    }

    /// Member completions after `.`, otherwise scope completions followed by Java keywords.
    pub fn completions(&self, offset: usize) -> Vec<Completion> {
        let at_member_access = ProjectIndex::at_member_access(&self.current.syntax, offset);
        let semantic = if at_member_access {
            self.index.member_completions(
                &self.current.syntax,
                self.current.resolved,
                self.current.file,
                offset,
            )
        } else {
            self.index.scope_completions(
                &self.current.syntax,
                self.current.resolved,
                self.current.file,
                offset,
            )
        };
        let mut completions: Vec<_> = semantic
            .into_iter()
            .map(|completion| Completion {
                label: completion.label,
                kind: completion.kind.into(),
                detail: completion.detail,
            })
            .collect();
        if !at_member_access {
            completions.extend(JAVA_KEYWORDS.iter().map(|keyword| Completion {
                label: (*keyword).to_owned(),
                kind: CompletionKind::Keyword,
                detail: String::new(),
            }));
        }
        completions
    }

    /// Signature help for the call containing `offset`.
    pub fn signature_help(&self, offset: usize) -> Option<jals_hir::SignatureHelp> {
        self.index.signature_help(
            &self.current.syntax,
            self.current.resolved,
            self.current.file,
            offset,
        )
    }

    /// Highlights for the symbol at `offset`, in document order.
    pub fn highlights(&self, offset: usize) -> Vec<Highlight> {
        let Some(target) = self.ident_at(offset) else {
            return Vec::new();
        };
        let anchor = usize::from(target.text_range().start());

        if let Some(id) = self.current.resolved.symbol_at(anchor) {
            return self
                .current
                .resolved
                .occurrences(id, true)
                .into_iter()
                .map(|range| self.highlight_at(range))
                .collect();
        }
        if let Some(item) = self.cross_file_type_at(anchor) {
            return self
                .current
                .resolved
                .references
                .iter()
                .filter(|reference| {
                    reference.namespace == Namespace::Type
                        && reference.resolution == Resolution::Unresolved
                        && reference.name == target.text()
                })
                .filter(|reference| {
                    self.index
                        .resolve_reference(self.current.file, reference)
                        .project_id()
                        == Some(item)
                })
                .map(|reference| self.highlight_at(reference.range.clone()))
                .collect();
        }
        self.current
            .syntax
            .descendants_with_tokens()
            .filter_map(SyntaxElement::into_token)
            .filter(|token| token.kind() == SyntaxKind::IDENT && token.text() == target.text())
            .map(|token| Highlight {
                range: Self::text_range(token.text_range()),
                kind: HighlightKind::of_token(&token),
            })
            .collect()
    }

    fn member_definition(&self, offset: usize) -> Option<(FileId, Range<usize>)> {
        let token = self.ident_at(offset)?;
        let field_access = token
            .parent()
            .filter(|parent| parent.kind() == SyntaxKind::FIELD_ACCESS)?;
        let access = ast::FieldAccess::cast(field_access.clone())?;
        let name = access.field()?;
        let receiver = access.receiver()?;
        let namespace =
            if field_access.parent().map(|parent| parent.kind()) == Some(SyntaxKind::CALL_EXPR) {
                Namespace::Method
            } else {
                Namespace::Value
            };
        let inference = jals_hir::TypeInference::infer(
            &self.current.syntax,
            self.current.resolved,
            self.index,
            self.current.file,
        );
        let owner = inference
            .type_of_expr(Self::text_range(receiver.syntax().text_range()))?
            .project_id()?;
        let member = self
            .index
            .member(self.index.resolve_member(owner, &name, namespace)?);
        Some(
            member
                .source_location
                .clone()
                .unwrap_or_else(|| (member.file, member.name_range.clone())),
        )
    }

    fn cross_file_type_at(&self, anchor: usize) -> Option<ItemId> {
        let reference = self.current.resolved.reference_at(anchor)?;
        (reference.namespace == Namespace::Type)
            .then(|| self.index.resolve_reference(self.current.file, reference))?
            .project_id()
    }

    fn item_references<'b>(
        &self,
        item: ItemId,
        include_declaration: bool,
        files: impl IntoIterator<Item = QueryFile<'b>>,
    ) -> Vec<FileRange> {
        let mut ranges = Vec::new();
        for source in files {
            for reference in &source.resolved.references {
                if reference.namespace != Namespace::Type {
                    continue;
                }
                let hit = match reference.resolution {
                    Resolution::Def(id) => {
                        self.index
                            .item_by_decl(source.file, source.resolved.def(id).name_range.start)
                            == Some(item)
                    }
                    Resolution::Unresolved => matches!(
                        self.index.resolve_reference(source.file, reference),
                        TypeResolution::Project(target) if target == item
                    ),
                };
                if hit {
                    ranges.push(FileRange {
                        file: source.file,
                        range: reference.range.clone(),
                    });
                }
            }
        }
        if include_declaration && let Some(declaration) = self.item_location(item) {
            ranges.push(declaration);
        }
        ranges.sort_by(|left, right| {
            left.file
                .cmp(&right.file)
                .then(left.range.start.cmp(&right.range.start))
                .then(left.range.end.cmp(&right.range.end))
        });
        ranges
    }

    fn item_location(&self, item: ItemId) -> Option<FileRange> {
        let item = self.index.item(item);
        let (file, range) = match item.origin {
            ItemOrigin::Project | ItemOrigin::Source => (item.file, item.name_range.clone()),
            ItemOrigin::Classpath => item.source_location.clone()?,
            ItemOrigin::Stdlib => return None,
        };
        Some(FileRange { file, range })
    }

    fn highlight_at(&self, range: Range<usize>) -> Highlight {
        let kind = self
            .ident_at(range.start)
            .map_or(HighlightKind::Read, |token| HighlightKind::of_token(&token));
        Highlight { range, kind }
    }

    /// The `IDENT` token at `offset` in the current file, preferring it at a token boundary (so a
    /// cursor at the end of a word still anchors to it). `offset` is clamped into the file's range.
    fn ident_at(&self, offset: usize) -> Option<SyntaxToken> {
        let root = &self.current.syntax;
        let end = usize::from(root.text_range().end());
        let offset = u32::try_from(offset.min(end)).unwrap_or(u32::MAX);
        root.token_at_offset(offset.into())
            .find(|token| token.kind() == SyntaxKind::IDENT)
    }

    /// A `text_size::TextRange` as a plain byte `Range<usize>`.
    fn text_range(range: text_size::TextRange) -> Range<usize> {
        usize::from(range.start())..usize::from(range.end())
    }
}

impl From<DefKind> for CompletionKind {
    fn from(kind: DefKind) -> Self {
        use DefKind::{
            AnnotationType, CatchParam, Class, Constructor, Enum, EnumConstant, Field, Interface,
            LambdaParam, Local, Method, Param, PatternVar, Record, Resource, TypeParam,
        };
        match kind {
            Method | Constructor => Self::Method,
            Field => Self::Field,
            EnumConstant => Self::EnumMember,
            Local | Param | LambdaParam | CatchParam | Resource | PatternVar => Self::Variable,
            TypeParam => Self::TypeParameter,
            Class | Record => Self::Class,
            Interface | AnnotationType => Self::Interface,
            Enum => Self::Enum,
        }
    }
}

impl HighlightKind {
    /// Write for declaration/binding names and mutating uses; Read for everything else.
    fn of_token(token: &SyntaxToken) -> Self {
        use SyntaxKind::{
            ANNOTATION_TYPE_DECL, CATCH_CLAUSE, CLASS_DECL, CONSTRUCTOR_DECL, ENUM_CONSTANT,
            ENUM_DECL, FIELD_DECL, FOR_EACH_STMT, INTERFACE_DECL, LOCAL_VAR_DECL, METHOD_DECL,
            NAME_REF, PARAM, RECORD_COMPONENT, RECORD_DECL, RESOURCE, TYPE_PARAM, TYPE_PATTERN,
        };

        /// A simple name reference is a write when it is the target of an assignment or the operand
        /// of `++`/`--`. Only simple names count: `o.f = 1` keeps `f` (under `FIELD_ACCESS`) a read.
        fn is_write_name_ref(name_ref: &SyntaxNode) -> bool {
            use SyntaxKind::{ASSIGNMENT_EXPR, MINUS_MINUS, PLUS_PLUS, POSTFIX_EXPR, UNARY_EXPR};
            match name_ref.parent() {
                Some(parent) if parent.kind() == ASSIGNMENT_EXPR => {
                    parent.children().next().as_ref() == Some(name_ref)
                }
                Some(parent) if parent.kind() == POSTFIX_EXPR => true,
                Some(parent) if parent.kind() == UNARY_EXPR => parent
                    .children_with_tokens()
                    .filter_map(SyntaxElement::into_token)
                    .any(|token| matches!(token.kind(), PLUS_PLUS | MINUS_MINUS)),
                _ => false,
            }
        }

        let Some(parent) = token.parent() else {
            return Self::Read;
        };
        match parent.kind() {
            CLASS_DECL | RECORD_DECL | INTERFACE_DECL | ANNOTATION_TYPE_DECL | ENUM_DECL
            | METHOD_DECL | CONSTRUCTOR_DECL | TYPE_PARAM | PARAM | RECORD_COMPONENT
            | ENUM_CONSTANT | FIELD_DECL | LOCAL_VAR_DECL | RESOURCE | CATCH_CLAUSE
            | TYPE_PATTERN | FOR_EACH_STMT => Self::Write,
            NAME_REF if is_write_name_ref(&parent) => Self::Write,
            _ => Self::Read,
        }
    }
}

const JAVA_KEYWORDS: &[&str] = &[
    "abstract",
    "assert",
    "boolean",
    "break",
    "byte",
    "case",
    "catch",
    "char",
    "class",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extends",
    "final",
    "finally",
    "float",
    "for",
    "goto",
    "if",
    "implements",
    "import",
    "instanceof",
    "int",
    "interface",
    "long",
    "native",
    "new",
    "package",
    "private",
    "protected",
    "public",
    "return",
    "short",
    "static",
    "strictfp",
    "super",
    "switch",
    "synchronized",
    "this",
    "throw",
    "throws",
    "transient",
    "try",
    "void",
    "volatile",
    "while",
    "true",
    "false",
    "null",
    "var",
    "yield",
    "record",
    "sealed",
    "permits",
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use core::cell::Cell;
    use jals_syntax::Parse;

    struct Fixture {
        roots: Vec<(FileId, SyntaxNode)>,
        resolved: Vec<Resolved>,
        index: ProjectIndex,
    }

    impl Fixture {
        fn new(files: &[&str]) -> Self {
            let roots: Vec<_> = files
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    (
                        FileId(u32::try_from(index).expect("test file index fits u32")),
                        Parse::parse(text).syntax(),
                    )
                })
                .collect();
            let resolved = roots
                .iter()
                .map(|(_, root)| Resolved::resolve_node(root))
                .collect();
            let index = ProjectIndex::builder(&roots).with_stdlib().build();
            Self {
                roots,
                resolved,
                index,
            }
        }

        fn queries(&self, file: usize) -> ProjectQueries<'_> {
            ProjectQueries::new(
                &self.index,
                QueryFile::new(
                    self.roots[file].0,
                    self.roots[file].1.clone(),
                    &self.resolved[file],
                ),
            )
        }

        fn files(&self) -> impl Iterator<Item = QueryFile<'_>> {
            self.roots
                .iter()
                .zip(&self.resolved)
                .map(|((id, root), resolved)| QueryFile::new(*id, root.clone(), resolved))
        }
    }

    #[test]
    fn definition_prefers_local_then_cross_file_and_member() {
        let files = [
            "package p; class Box { int size; }",
            "package p; class Use { void f(Box b) { int n = b.size; use(n); } }",
        ];
        let fixture = Fixture::new(&files);
        let queries = fixture.queries(1);
        assert_eq!(
            queries.definition(files[1].find("Box").unwrap()),
            Some(FileRange {
                file: FileId(0),
                range: 17..20
            })
        );
        assert_eq!(
            queries.definition(files[1].find("size").unwrap()),
            Some(FileRange {
                file: FileId(0),
                range: 27..31
            })
        );
        assert_eq!(
            queries.definition(files[1].rfind('n').unwrap()),
            Some(FileRange {
                file: FileId(1),
                range: 43..44
            })
        );
    }

    #[test]
    fn project_references_are_sorted_and_declaration_is_optional() {
        let files = ["package p; class A {}", "package p; class B { A a; }"];
        let fixture = Fixture::new(&files);
        let offset = files[1].find('A').unwrap();
        assert_eq!(
            fixture.queries(1).references(offset, true, fixture.files()),
            [
                FileRange {
                    file: FileId(0),
                    range: 17..18
                },
                FileRange {
                    file: FileId(1),
                    range: 21..22
                },
            ]
        );
        assert_eq!(
            fixture
                .queries(1)
                .references(offset, false, fixture.files()),
            [FileRange {
                file: FileId(1),
                range: 21..22
            }]
        );
    }

    #[test]
    fn local_references_do_not_consume_the_project_iterator() {
        let text = "class C { void f() { int x = 0; use(x); } }";
        let fixture = Fixture::new(&[text]);
        let consumed = Cell::new(0);
        let files = fixture
            .files()
            .inspect(|_| consumed.set(consumed.get() + 1));
        let ranges = fixture
            .queries(0)
            .references(text.find("x = 0").unwrap(), true, files);
        assert_eq!(ranges.len(), 2);
        assert_eq!(consumed.get(), 0);
    }

    #[test]
    fn cross_file_highlight_does_not_match_a_same_spelled_local() {
        let files = [
            "package p; class A {}",
            "package p; class B { A value; void f() { int A = 0; use(A); } }",
        ];
        let fixture = Fixture::new(&files);
        let type_offset = files[1].find("A value").unwrap();
        assert_eq!(
            fixture.queries(1).highlights(type_offset),
            [Highlight {
                range: type_offset..type_offset + 1,
                kind: HighlightKind::Read,
            }]
        );
    }

    #[test]
    fn member_completion_excludes_keywords_and_bare_completion_includes_them() {
        let text = "class Box { int size; void f(Box box) { box. } }";
        let fixture = Fixture::new(&[text]);
        let member = fixture
            .queries(0)
            .completions(text.find("box.").unwrap() + "box.".len());
        assert!(
            member
                .iter()
                .any(|item| { item.label == "size" && item.kind == CompletionKind::Field })
        );
        assert!(
            member
                .iter()
                .all(|item| item.kind != CompletionKind::Keyword)
        );

        let bare = fixture.queries(0).completions(text.find("box)").unwrap());
        assert!(
            bare.iter()
                .any(|item| { item.label == "return" && item.kind == CompletionKind::Keyword })
        );
    }

    #[test]
    fn source_less_stdlib_type_has_references_but_no_declaration_target() {
        let text = "class C { String first; String second; }";
        let fixture = Fixture::new(&[text]);
        let ranges =
            fixture
                .queries(0)
                .references(text.find("String").unwrap(), true, fixture.files());
        assert_eq!(
            ranges,
            [
                FileRange {
                    file: FileId(0),
                    range: 10..16,
                },
                FileRange {
                    file: FileId(0),
                    range: 24..30,
                },
            ]
        );
        assert!(
            fixture
                .queries(0)
                .definition(text.find("String").unwrap())
                .is_none()
        );
    }

    #[test]
    fn hover_completion_signature_and_highlight_policies_are_shared() {
        let text = "class C { int area(int w, int h) { return w; } void f() { int x = 1; x++; area(x, ); } }";
        let fixture = Fixture::new(&[text]);
        let queries = fixture.queries(0);
        assert_eq!(
            queries
                .hover(text.find('1').unwrap())
                .map(|ty| ty.to_string()),
            Some("int".to_owned())
        );
        let completions = queries.completions(text.find("x = 1").unwrap());
        assert!(
            completions
                .iter()
                .any(|item| item.label == "return" && item.kind == CompletionKind::Keyword)
        );
        let help = queries
            .signature_help(text.find("area(x, ").unwrap() + "area(x, ".len())
            .unwrap();
        assert_eq!(help.active_parameter, 1);
        let highlights = queries.highlights(text.find("x = 1").unwrap());
        assert_eq!(
            highlights
                .iter()
                .map(|highlight| highlight.kind)
                .collect::<Vec<_>>(),
            [
                HighlightKind::Write,
                HighlightKind::Write,
                HighlightKind::Read
            ]
        );
    }

    #[test]
    fn unresolved_names_fall_back_lexically_and_bad_offsets_are_safe() {
        let text = "class C { Missing x; void f() { use(Missing); } }";
        let fixture = Fixture::new(&[text]);
        let queries = fixture.queries(0);
        assert_eq!(queries.highlights(text.find("Missing").unwrap()).len(), 2);
        assert!(queries.definition(usize::MAX).is_none());
        assert!(queries.highlights(usize::MAX).is_empty());
    }
}
