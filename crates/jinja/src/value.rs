//! The values a template reads.
//!
//! [`Value`] is a cheap-to-clone handle over one of a fixed set of shapes — undefined, none, a
//! boolean, a number, a string, a sequence, a map, or a consumer-supplied [`Object`]. Everything
//! larger than a machine word is behind an [`Rc`], so passing a value into a loop body copies a
//! pointer.
//!
//! Two things differ from minijinja on purpose, and both are what let a consumer keep a domain rule
//! in its own crate:
//!
//! - **A lookup that finds nothing returns [`None`], not undefined.** minijinja folds the two
//!   together; keeping them apart is what makes [`Environment::set_strict_variables`] expressible,
//!   because *no such key* (a typo) and *a key holding nothing* (a value to spell a `| default(…)`
//!   for) stop being the same answer.
//! - **[`Object`] is not `Send + Sync`.** Nothing here crosses a thread, so requiring it would only
//!   cost implementors an `Arc` they have no use for.
//!
//! [`Environment::set_strict_variables`]: crate::Environment::set_strict_variables

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;

/// What shape a [`Value`] has.
///
/// An [`Object`] reports the kind its [`Object::enumerate`] implies: [`Self::Seq`] when it
/// enumerates and [`Self::Map`] when it does not, so a consumer branching on the kind never has to
/// know whether a value is native or supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ValueKind {
    /// Known, and not set.
    Undefined,
    /// Explicitly nothing — Jinja's `none`.
    None,
    /// `true` or `false`.
    Bool,
    /// An integer or a float.
    Number,
    /// Text.
    String,
    /// An ordered run of values.
    Seq,
    /// String keys to values, iterated in key order.
    Map,
}

impl ValueKind {
    /// The kind as a noun phrase carrying its own article, which is how a message names it:
    /// `a map has no text form`, `an undefined value cannot be iterated`.
    pub(crate) const fn phrase(self) -> &'static str {
        match self {
            Self::Undefined => "an undefined value",
            Self::None => "none",
            Self::Bool => "a boolean",
            Self::Number => "a number",
            Self::String => "a string",
            Self::Seq => "a sequence",
            Self::Map => "a map",
        }
    }
}

impl fmt::Display for ValueKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Undefined => "undefined",
            Self::None => "none",
            Self::Bool => "bool",
            Self::Number => "number",
            Self::String => "string",
            Self::Seq => "sequence",
            Self::Map => "map",
        })
    }
}

/// What a value a template can read holds.
#[derive(Clone)]
enum Repr {
    Undefined,
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(Rc<str>),
    Seq(Rc<[Value]>),
    Map(Rc<BTreeMap<String, Value>>),
    Object(Rc<dyn Object>),
}

/// A value a template can read.
#[derive(Clone)]
pub struct Value(Repr);

impl Value {
    /// Known, and not set. Emitting this is what [`UndefinedBehavior`] decides the fate of.
    ///
    /// [`UndefinedBehavior`]: crate::UndefinedBehavior
    pub const UNDEFINED: Self = Self(Repr::Undefined);

    /// Explicitly nothing — Jinja's `none`, which renders as `none` and is falsy.
    pub const NONE: Self = Self(Repr::None);

    /// Wrap a consumer-supplied object, which answers lookups and iteration however it likes.
    pub fn from_object<T: Object + 'static>(object: T) -> Self {
        Self(Repr::Object(Rc::new(object)))
    }

    /// Wrap an object already behind an [`Rc`], for a consumer that shares one across renders.
    pub fn from_dyn_object(object: Rc<dyn Object>) -> Self {
        Self(Repr::Object(object))
    }

    /// A map from anything that yields `(key, value)` pairs.
    pub fn from_entries<K: Into<String>, I: IntoIterator<Item = (K, Self)>>(entries: I) -> Self {
        Self(Repr::Map(Rc::new(
            entries
                .into_iter()
                .map(|(key, value)| (key.into(), value))
                .collect(),
        )))
    }

    /// What shape this value has.
    pub fn kind(&self) -> ValueKind {
        match &self.0 {
            Repr::Undefined => ValueKind::Undefined,
            Repr::None => ValueKind::None,
            Repr::Bool(_) => ValueKind::Bool,
            Repr::Int(_) | Repr::Float(_) => ValueKind::Number,
            Repr::Str(_) => ValueKind::String,
            Repr::Seq(_) => ValueKind::Seq,
            Repr::Map(_) => ValueKind::Map,
            Repr::Object(object) => match object.enumerate() {
                Enumerator::Values(_) => ValueKind::Seq,
                Enumerator::NonEnumerable => ValueKind::Map,
            },
        }
    }

    /// Whether this is the undefined value.
    pub const fn is_undefined(&self) -> bool {
        matches!(self.0, Repr::Undefined)
    }

    /// Whether this is Jinja's `none`.
    pub const fn is_none(&self) -> bool {
        matches!(self.0, Repr::None)
    }

    /// Whether a condition holding this value takes its arm.
    ///
    /// Jinja's rule: undefined and none are false, a number is false at zero, and a string,
    /// sequence, or map is false when empty. An object is as true as its enumeration is non-empty,
    /// and one that does not enumerate is true.
    pub fn is_true(&self) -> bool {
        match &self.0 {
            Repr::Undefined | Repr::None => false,
            Repr::Bool(value) => *value,
            Repr::Int(value) => *value != 0,
            Repr::Float(value) => *value != 0.0,
            Repr::Str(text) => !text.is_empty(),
            Repr::Seq(items) => !items.is_empty(),
            Repr::Map(map) => !map.is_empty(),
            Repr::Object(object) => match object.enumerate() {
                Enumerator::Values(items) => !items.is_empty(),
                Enumerator::NonEnumerable => true,
            },
        }
    }

    /// The text, if this is a string. A number is *not* borrowed as text; render it instead.
    pub fn as_str(&self) -> Option<&str> {
        match &self.0 {
            Repr::Str(text) => Some(text),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub const fn as_i64(&self) -> Option<i64> {
        match self.0 {
            Repr::Int(value) => Some(value),
            _ => None,
        }
    }

    /// How many items this holds, for the shapes where that is a question.
    pub fn len(&self) -> Option<usize> {
        match &self.0 {
            Repr::Str(text) => Some(text.chars().count()),
            Repr::Seq(items) => Some(items.len()),
            Repr::Map(map) => Some(map.len()),
            Repr::Object(object) => match object.enumerate() {
                Enumerator::Values(items) => Some(items.len()),
                Enumerator::NonEnumerable => None,
            },
            _ => None,
        }
    }

    /// Whether this holds nothing, for the shapes [`Self::len`] answers for.
    pub fn is_empty(&self) -> Option<bool> {
        self.len().map(|length| length == 0)
    }

    /// Look one key up, by the name a `.` path or a `[…]` index spells.
    ///
    /// [`None`] means *there is no such key*, which is the answer
    /// [`Environment::set_strict_variables`] turns into an error. A key that exists and holds
    /// nothing answers <code>Some([Value::UNDEFINED])</code> instead, and is the case `| default(…)` is
    /// written for.
    ///
    /// A sequence answers for a decimal index, so `{{ items["0"] }}` and `{{ items[0] }}` are the
    /// same access.
    ///
    /// [`Environment::set_strict_variables`]: crate::Environment::set_strict_variables
    pub fn get_attr(&self, key: &str) -> Option<Self> {
        match &self.0 {
            Repr::Map(map) => map.get(key).cloned(),
            Repr::Seq(items) => key
                .parse::<usize>()
                .ok()
                .and_then(|at| items.get(at).cloned()),
            Repr::Object(object) => object.get_value(key),
            _ => None,
        }
    }

    /// Every key this value answers for, when it is a map. Sequences and objects answer [`None`]:
    /// a sequence's keys are its indices, and only an object knows what it would accept.
    ///
    /// This is what lets an unknown name say what the known ones were.
    pub fn keys(&self) -> Option<Vec<&str>> {
        match &self.0 {
            Repr::Map(map) => Some(map.keys().map(String::as_str).collect()),
            _ => None,
        }
    }

    /// The values a `{% for %}` walks, or [`None`] when this shape cannot be iterated.
    ///
    /// A map yields its keys, in key order, as Jinja does. A sequence yields its items. An object
    /// yields whatever it enumerates.
    pub fn try_iter(&self) -> Option<Vec<Self>> {
        match &self.0 {
            Repr::Seq(items) => Some(items.to_vec()),
            Repr::Map(map) => Some(map.keys().map(|key| Self::from(key.as_str())).collect()),
            Repr::Object(object) => match object.enumerate() {
                Enumerator::Values(items) => Some(items),
                Enumerator::NonEnumerable => None,
            },
            _ => None,
        }
    }

    /// Append this value's text form, or say why it has none.
    ///
    /// `Ok(false)` means the value is undefined and the caller decides what that means; a shape
    /// with no text form at all is `Err`.
    pub(crate) fn write(&self, out: &mut String) -> Result<bool, ValueKind> {
        use core::fmt::Write as _;

        match &self.0 {
            Repr::Undefined => return Ok(false),
            Repr::None => out.push_str("none"),
            Repr::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Repr::Int(value) => {
                let _ = write!(out, "{value}");
            }
            Repr::Float(value) => {
                let _ = write!(out, "{value}");
            }
            Repr::Str(text) => out.push_str(text),
            Repr::Seq(_) | Repr::Map(_) | Repr::Object(_) => return Err(self.kind()),
        }
        Ok(true)
    }

    /// The text form, for a filter that needs one. Undefined and the collections have none.
    pub(crate) fn to_text(&self) -> Option<String> {
        let mut out = String::new();
        match self.write(&mut out) {
            Ok(true) => Some(out),
            _ => None,
        }
    }
}

impl fmt::Debug for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.0 {
            Repr::Undefined => f.write_str("undefined"),
            Repr::None => f.write_str("none"),
            Repr::Bool(value) => fmt::Debug::fmt(value, f),
            Repr::Int(value) => fmt::Debug::fmt(value, f),
            Repr::Float(value) => fmt::Debug::fmt(value, f),
            Repr::Str(text) => fmt::Debug::fmt(text, f),
            Repr::Seq(items) => f.debug_list().entries(items.iter()).finish(),
            Repr::Map(map) => f.debug_map().entries(map.iter()).finish(),
            Repr::Object(object) => fmt::Debug::fmt(object, f),
        }
    }
}

impl PartialEq for Value {
    /// Equal within a shape, and never across two. Two objects are equal when they are the same
    /// object, since only the implementor knows what else could mean.
    ///
    /// Floats compare by [`f64::total_cmp`], so `NaN` equals itself and `-0.0` does not equal
    /// `0.0` — the ordering a sort would use, rather than IEEE's.
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Repr::Undefined, Repr::Undefined) | (Repr::None, Repr::None) => true,
            (Repr::Bool(left), Repr::Bool(right)) => left == right,
            (Repr::Int(left), Repr::Int(right)) => left == right,
            (Repr::Float(left), Repr::Float(right)) => left.total_cmp(right).is_eq(),
            (Repr::Str(left), Repr::Str(right)) => left == right,
            (Repr::Seq(left), Repr::Seq(right)) => left == right,
            (Repr::Map(left), Repr::Map(right)) => left == right,
            (Repr::Object(left), Repr::Object(right)) => Rc::ptr_eq(left, right),
            _ => false,
        }
    }
}

impl PartialOrd for Value {
    /// Ordered within a shape, and incomparable across two — which is what makes `<` on a string
    /// and a number answer nothing rather than answer wrongly.
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        match (&self.0, &other.0) {
            (Repr::Bool(left), Repr::Bool(right)) => Some(left.cmp(right)),
            (Repr::Int(left), Repr::Int(right)) => Some(left.cmp(right)),
            (Repr::Float(left), Repr::Float(right)) => Some(left.total_cmp(right)),
            (Repr::Str(left), Repr::Str(right)) => Some(left.as_ref().cmp(right.as_ref())),
            _ => None,
        }
    }
}

impl From<bool> for Value {
    fn from(value: bool) -> Self {
        Self(Repr::Bool(value))
    }
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self(Repr::Int(value))
    }
}

impl From<i32> for Value {
    fn from(value: i32) -> Self {
        Self(Repr::Int(i64::from(value)))
    }
}

impl From<u32> for Value {
    fn from(value: u32) -> Self {
        Self(Repr::Int(i64::from(value)))
    }
}

impl From<usize> for Value {
    /// Saturating, because a length that does not fit in an `i64` is not a number a template is
    /// about to render correctly either way.
    fn from(value: usize) -> Self {
        Self(Repr::Int(i64::try_from(value).unwrap_or(i64::MAX)))
    }
}

impl From<f64> for Value {
    fn from(value: f64) -> Self {
        Self(Repr::Float(value))
    }
}

impl From<&str> for Value {
    fn from(value: &str) -> Self {
        Self(Repr::Str(Rc::from(value)))
    }
}

impl From<String> for Value {
    fn from(value: String) -> Self {
        Self(Repr::Str(Rc::from(value.as_str())))
    }
}

impl From<Rc<str>> for Value {
    fn from(value: Rc<str>) -> Self {
        Self(Repr::Str(value))
    }
}

impl From<Vec<Self>> for Value {
    fn from(value: Vec<Self>) -> Self {
        Self(Repr::Seq(Rc::from(value)))
    }
}

impl From<BTreeMap<String, Self>> for Value {
    fn from(value: BTreeMap<String, Self>) -> Self {
        Self(Repr::Map(Rc::new(value)))
    }
}

impl<T: Into<Self>> FromIterator<T> for Value {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(Repr::Seq(iter.into_iter().map(Into::into).collect()))
    }
}

/// What an [`Object`] yields when a `{% for %}` walks it.
#[derive(Debug, Clone, PartialEq)]
pub enum Enumerator {
    /// This object is a namespace, not a sequence: iterating it is an error.
    NonEnumerable,
    /// These values, in this order.
    Values(Vec<Value>),
}

/// A value whose behaviour a consumer supplies.
///
/// This is the seam a domain rule lives behind. A set that answers *membership* for any name at
/// all, a lazily computed namespace, a handle onto something the engine has no business knowing
/// about — each is an `Object` in the consumer's crate rather than a variant here.
///
/// Unlike minijinja's, it is neither `Send` nor `Sync`, and its keys are `&str` rather than
/// [`Value`]: nothing here crosses a thread, and every key a template can spell is a string.
pub trait Object: fmt::Debug {
    /// Answer one lookup. [`None`] is *there is no such key*; <code>Some([Value::UNDEFINED])</code>
    /// is a key that exists and holds nothing.
    fn get_value(&self, key: &str) -> Option<Value>;

    /// What a `{% for %}` over this object walks. Namespaces leave this alone.
    fn enumerate(&self) -> Enumerator {
        Enumerator::NonEnumerable
    }
}
