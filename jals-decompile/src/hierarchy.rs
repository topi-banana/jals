//! Conservative source-level hierarchy queries used by method-body decompilation.
//!
//! A JVM symbolic reference records the hierarchy that existed when a client class was compiled.
//! Re-rendering that reference as Java source must instead satisfy the hierarchy loaded now. This
//! index proves the small set of facts needed for `Interface.super.m()` and declines the rendering
//! whenever a relevant type, edge, or method relationship is incomplete or ambiguous.

use alloc::borrow::{Cow, ToOwned};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use jals_classfile::{
    AttributeBody, ClassAccessFlags, ClassFile, MethodAccessFlags, MethodDescriptor, MethodInfo,
};

const OBJECT: &str = "java/lang/Object";

enum ClassEntry<'a> {
    Unique(&'a ClassFile),
    Ambiguous,
}

struct MethodDeclaration<'a> {
    owner: String,
    method: &'a MethodInfo,
    descriptor: MethodDescriptor,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Visit {
    Visiting,
    Done,
}

/// An immutable index over the class files available to a decompilation pass.
///
/// Duplicate binary names are deliberately ambiguous. Qualified interface-super rendering uses
/// this index only when every relevant hierarchy fact resolves uniquely; ordinary bytecode
/// decompilation remains available with an incomplete index.
pub struct ClassHierarchy<'a> {
    classes: BTreeMap<String, ClassEntry<'a>>,
}

impl<'a> ClassHierarchy<'a> {
    /// Index `classes` by internal binary name without choosing between duplicate definitions.
    #[must_use]
    pub fn new(classes: &'a [ClassFile]) -> Self {
        let mut indexed = BTreeMap::new();
        for cf in classes {
            let Some(name) = Self::class_name(cf) else {
                continue;
            };
            match indexed.entry(name) {
                alloc::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(ClassEntry::Unique(cf));
                }
                alloc::collections::btree_map::Entry::Occupied(mut entry) => {
                    entry.insert(ClassEntry::Ambiguous);
                }
            }
        }
        Self { classes: indexed }
    }

    /// Whether `qualifier.super.name(...)` is source-legal under the currently loaded hierarchy.
    pub(crate) fn allows_interface_super(
        &self,
        current: &ClassFile,
        qualifier: &str,
        name: &str,
        descriptor: &str,
    ) -> bool {
        let Some(current_name) = Self::class_name(current) else {
            return false;
        };
        if !self
            .unique(&current_name)
            .is_some_and(|indexed| core::ptr::eq(indexed, current))
            || Self::has_signature(current)
        {
            return false;
        }

        let Some(qualifier_cf) = self.unique(qualifier) else {
            return false;
        };
        if !qualifier_cf.access_flags.is_interface()
            || !Self::type_accessible_from(qualifier_cf, qualifier, &current_name)
        {
            return false;
        }

        let Some(direct) = self.direct_supertypes(&current_name) else {
            return false;
        };
        if !direct.iter().any(|supertype| supertype == qualifier) {
            return false;
        }

        // JLS 15.12.1: no other source-level direct supertype may be a subtype of the qualifier.
        for other in direct.iter().filter(|other| other.as_str() != qualifier) {
            let Some(is_subtype) = self.is_subtype(other, qualifier) else {
                return false;
            };
            if is_subtype {
                return false;
            }
        }

        let Some(selected) = self.interface_declaration(qualifier, name, descriptor) else {
            return false;
        };

        // JLS 15.12.3: another direct supertype must not contribute a distinct override of the
        // compile-time declaration selected through the qualifier.
        for other in direct.iter().filter(|other| other.as_str() != qualifier) {
            if self.contributes_override(other, name, &selected) != Some(false) {
                return false;
            }
        }
        true
    }

    fn unique(&self, name: &str) -> Option<&'a ClassFile> {
        match self.classes.get(name)? {
            ClassEntry::Unique(cf) => Some(*cf),
            ClassEntry::Ambiguous => None,
        }
    }

    fn class_name(cf: &ClassFile) -> Option<String> {
        cf.constant_pool
            .class_name(cf.this_class)
            .map(Cow::into_owned)
    }

    /// Validated source-level direct supertypes (superclass first, then superinterfaces).
    fn direct_supertypes(&self, name: &str) -> Option<Vec<String>> {
        if name == OBJECT {
            return Some(Vec::new());
        }
        let cf = self.unique(name)?;
        let pool = &cf.constant_pool;
        let mut direct = Vec::new();
        let mut seen = BTreeSet::new();

        if cf.access_flags.is_interface() {
            if cf.super_class == 0 || pool.class_name(cf.super_class)?.as_ref() != OBJECT {
                return None;
            }
        } else {
            if cf.super_class == 0 {
                return None;
            }
            let superclass = pool.class_name(cf.super_class)?.into_owned();
            if superclass != OBJECT && self.unique(&superclass)?.access_flags.is_interface() {
                return None;
            }
            seen.insert(superclass.clone());
            direct.push(superclass);
        }

        for &index in &cf.interfaces {
            let interface = pool.class_name(index)?.into_owned();
            if !seen.insert(interface.clone())
                || !self.unique(&interface)?.access_flags.is_interface()
            {
                return None;
            }
            direct.push(interface);
        }
        Some(direct)
    }

    /// The complete, validated transitive supertype closure, including `start` itself.
    fn closure(&self, start: &str) -> Option<BTreeSet<String>> {
        let mut states: BTreeMap<String, Visit> = BTreeMap::new();
        let mut closure = BTreeSet::new();
        let mut stack = alloc::vec![(start.to_owned(), false)];

        while let Some((name, exiting)) = stack.pop() {
            if exiting {
                states.insert(name, Visit::Done);
                continue;
            }
            match states.get(&name) {
                Some(Visit::Visiting) => return None,
                Some(Visit::Done) => continue,
                _ => {}
            }
            states.insert(name.clone(), Visit::Visiting);
            closure.insert(name.clone());
            stack.push((name.clone(), true));
            let direct = self.direct_supertypes(&name)?;
            for parent in direct.into_iter().rev() {
                if states.get(&parent) == Some(&Visit::Visiting) {
                    return None;
                }
                if states.get(&parent) != Some(&Visit::Done) {
                    stack.push((parent, false));
                }
            }
        }
        Some(closure)
    }

    fn is_subtype(&self, subtype: &str, supertype: &str) -> Option<bool> {
        Some(self.closure(subtype)?.contains(supertype))
    }

    /// Resolve the one source declaration selected by `qualifier.super.name(...)`.
    fn interface_declaration(
        &self,
        qualifier: &str,
        name: &str,
        descriptor: &str,
    ) -> Option<MethodDeclaration<'a>> {
        let invocation = MethodDescriptor::parse(descriptor).ok()?;
        let closure = self.closure(qualifier)?;
        let mut candidates = Vec::new();

        for owner in &closure {
            if owner == OBJECT {
                continue;
            }
            let cf = self.unique(owner)?;
            if !cf.access_flags.is_interface() || Self::has_signature(cf) {
                return None;
            }
            for method in &cf.methods {
                let method_name = cf.constant_pool.utf8(method.name_index)?;
                if method_name.as_ref() != name {
                    continue;
                }
                if Self::has_signature_in(&method.attributes) {
                    return None;
                }
                let method_descriptor = cf.constant_pool.utf8(method.descriptor_index)?;
                let parsed = MethodDescriptor::parse(&method_descriptor).ok()?;
                if parsed.params != invocation.params {
                    continue;
                }
                let flags = method.access_flags;
                if flags.contains(MethodAccessFlags::PRIVATE) || flags.is_static() {
                    continue;
                }
                if !flags.is_public()
                    || flags.contains(MethodAccessFlags::BRIDGE)
                    || flags.contains(MethodAccessFlags::SYNTHETIC)
                {
                    return None;
                }
                candidates.push(MethodDeclaration {
                    owner: owner.clone(),
                    method,
                    descriptor: parsed,
                });
            }
        }

        let mut selected = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            let mut overridden = false;
            for (other_index, other) in candidates.iter().enumerate() {
                if index == other_index || candidate.owner == other.owner {
                    continue;
                }
                if self.is_subtype(&other.owner, &candidate.owner)? {
                    overridden = true;
                    break;
                }
            }
            if !overridden {
                selected.push(index);
            }
        }
        let [selected_index] = selected.as_slice() else {
            return None;
        };
        let selected = candidates.swap_remove(*selected_index);
        let flags = selected.method.access_flags;
        if selected.descriptor != invocation
            || flags.is_abstract()
            || flags.contains(MethodAccessFlags::NATIVE)
            || !selected
                .method
                .attributes
                .iter()
                .any(|attribute| matches!(attribute.body, AttributeBody::Code(_)))
        {
            return None;
        }
        Some(selected)
    }

    /// Whether a method distinct from `selected` is contributed by `direct` and overrides it.
    fn contributes_override(
        &self,
        direct: &str,
        name: &str,
        selected: &MethodDeclaration<'a>,
    ) -> Option<bool> {
        for owner in self.closure(direct)? {
            if owner == OBJECT {
                continue;
            }
            let cf = self.unique(&owner)?;
            if Self::has_signature(cf) {
                return None;
            }
            let contributes = self.is_subtype(&owner, &selected.owner)?;
            for method in &cf.methods {
                let method_name = cf.constant_pool.utf8(method.name_index)?;
                if method_name.as_ref() != name {
                    continue;
                }
                if Self::has_signature_in(&method.attributes) {
                    return None;
                }
                let descriptor = cf.constant_pool.utf8(method.descriptor_index)?;
                let descriptor = MethodDescriptor::parse(&descriptor).ok()?;
                if descriptor.params != selected.descriptor.params
                    || method.access_flags.is_static()
                    || method.access_flags.contains(MethodAccessFlags::PRIVATE)
                    || core::ptr::eq(method, selected.method)
                {
                    continue;
                }
                if !method.access_flags.is_public()
                    || method.access_flags.contains(MethodAccessFlags::BRIDGE)
                    || method.access_flags.contains(MethodAccessFlags::SYNTHETIC)
                {
                    return None;
                }
                if contributes {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn type_accessible_from(cf: &ClassFile, name: &str, from: &str) -> bool {
        cf.access_flags.contains(ClassAccessFlags::PUBLIC)
            || Self::package(name) == Self::package(from)
    }

    fn package(name: &str) -> &str {
        name.rsplit_once('/').map_or("", |(package, _)| package)
    }

    fn has_signature(cf: &ClassFile) -> bool {
        Self::has_signature_in(&cf.attributes)
    }

    fn has_signature_in(attributes: &[jals_classfile::Attribute]) -> bool {
        attributes
            .iter()
            .any(|attribute| matches!(attribute.body, AttributeBody::Signature { .. }))
    }
}
