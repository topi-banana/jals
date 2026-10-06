//! Maven-compatible version ordering and version requirements.
//!
//! Two things a Cargo-like front end needs over a Maven registry:
//!
//! - **Ordering** that matches `org.apache.maven.artifact.versioning.ComparableVersion`, because
//!   the registry's version list is not semver: `1.0 == 1`, `1.0-alpha-1 < 1.0`, `33.4.0-jre` is
//!   a release greater than `33.4.0`. The port below keeps Maven's item model (`1.0-rc1` parses
//!   as `1.0` followed by a list of `rc` and `1`) and the `alpha < beta < milestone < rc <
//!   snapshot < release < sp < unknown` qualifier order, so selection agrees with what a Maven
//!   user sees in their repository manager.
//! - **Requirements** with Cargo's spelling rule and Maven's range syntax: a bare `2.0.16` is a
//!   caret requirement (`>=2.0.16, <3.0.0`), while `[1.0,2.0)` is read verbatim with Maven
//!   semantics. That is the one place this crate deliberately mixes dialects, and it is what
//!   makes `commons-lang3 = "3.17.0"` behave the way both audiences expect.

use alloc::borrow::ToOwned;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::fmt;
use core::fmt::Write as _;
use core::hash::{Hash, Hasher};
use core::str::FromStr;

/// Maven's known qualifiers, in ascending order. The empty string is the release marker: a
/// version without a qualifier and a version with an explicit `ga`/`final`/`release` compare
/// equal, exactly as `ComparableVersion` treats them.
const QUALIFIERS: [&str; 7] = ["alpha", "beta", "milestone", "rc", "snapshot", "", "sp"];

/// The index of the release marker in [`QUALIFIERS`].
const RELEASE_INDEX: usize = 5;

/// A parsed version, ordered by Maven's `ComparableVersion` rules.
///
/// Equality is *semantic*: `1.0.0 == 1` and `1.0-ga == 1.0`. [`as_str`](Version::as_str) keeps
/// the spelling the manifest or registry used, so diagnostics and lock rendering show what a
/// reader wrote rather than a canonicalized form.
#[derive(Debug, Clone)]
pub struct Version {
    raw: String,
    items: Vec<Item>,
}

/// One item of a parsed version, mirroring `ComparableVersion.Item`.
#[derive(Debug, Clone)]
enum Item {
    /// A numeric segment, normalized: no leading zeros, never empty (`0`).
    Number(String),
    /// A qualifier segment, lowercased and alias-normalized.
    Text(String),
    /// A `-`-separated (or digit/letter-transition) sub-sequence, compared element-wise.
    List(Vec<Self>),
}

/// A parsed segment before it is materialized into the tree `Item`s.
enum RawItem {
    Number(String),
    Text(String),
    List(usize),
}

impl Version {
    /// Parse `raw` into a comparable version.
    ///
    /// # Errors
    /// [`VersionError::Empty`] for a blank string, [`VersionError::Whitespace`] for an embedded
    /// space (a version is one token, and accepting whitespace would let two spellings of one
    /// value compare differently in the lock).
    pub fn parse(raw: &str) -> Result<Self, VersionError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(VersionError::Empty {
                raw: raw.to_owned(),
            });
        }
        if raw.chars().any(char::is_whitespace) {
            return Err(VersionError::Whitespace {
                raw: raw.to_owned(),
            });
        }
        let items = Self::parse_items(&raw.to_lowercase());
        Ok(Self {
            raw: raw.to_owned(),
            items,
        })
    }

    /// The version as it was written.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The leading numeric components, at most three, normalized without leading zeros.
    /// Used by the caret/tilde/wildcard requirements to derive an upper bound;
    /// `33.4.0-jre` yields `["33", "4", "0"]`. Decimal strings rather than integers, because a
    /// Maven version has no width limit and bumping `u64::MAX` must not wrap.
    fn numeric_components(&self) -> Vec<String> {
        let mut components = Vec::new();
        for part in self.raw.split(['.', '-', '_']) {
            if components.len() == 3 {
                break;
            }
            if part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()) {
                break;
            }
            components.push(Self::normalize_number(part));
        }
        components
    }

    /// Parse into the item tree, porting `ComparableVersion.parseVersion`.
    fn parse_items(version: &str) -> Vec<Item> {
        let chars: Vec<char> = version.chars().collect();
        // Arena of lists; `current` is the list being filled, and a child list is referenced by
        // index until materialization. This is the stack of `ListItem`s in Maven's parser.
        let mut arena: Vec<Vec<RawItem>> = alloc::vec![Vec::new()];
        let mut current = 0usize;
        let mut is_digit = false;
        let mut start = 0usize;
        for (i, ch) in chars.iter().enumerate() {
            if *ch == '.' {
                if i == start {
                    arena[current].push(RawItem::Number("0".to_owned()));
                } else {
                    arena[current].push(Self::raw_item(is_digit, &chars[start..i]));
                }
                start = i + 1;
            } else if *ch == '-' {
                if i == start {
                    arena[current].push(RawItem::Number("0".to_owned()));
                } else {
                    arena[current].push(Self::raw_item(is_digit, &chars[start..i]));
                }
                start = i + 1;
                let child = arena.len();
                arena.push(Vec::new());
                arena[current].push(RawItem::List(child));
                current = child;
            } else if ch.is_ascii_digit() {
                if !is_digit && i > start {
                    let text: String = chars[start..i].iter().collect();
                    arena[current].push(RawItem::Text(Self::alias(&text, true).to_owned()));
                    start = i;
                    let child = arena.len();
                    arena.push(Vec::new());
                    arena[current].push(RawItem::List(child));
                    current = child;
                }
                is_digit = true;
            } else {
                if is_digit && i > start {
                    arena[current].push(Self::raw_item(true, &chars[start..i]));
                    start = i;
                    let child = arena.len();
                    arena.push(Vec::new());
                    arena[current].push(RawItem::List(child));
                    current = child;
                }
                is_digit = false;
            }
        }
        if chars.len() > start {
            arena[current].push(Self::raw_item(is_digit, &chars[start..]));
        }
        Self::materialize(&arena, 0)
    }

    /// One segment: digits become a normalized number, everything else a qualifier.
    fn raw_item(is_digit: bool, chars: &[char]) -> RawItem {
        let text: String = chars.iter().collect();
        if is_digit {
            RawItem::Number(Self::normalize_number(&text))
        } else {
            RawItem::Text(Self::alias(&text, false).to_owned())
        }
    }

    /// Materialize the arena, normalizing lists deepest-first as Maven does.
    fn materialize(arena: &[Vec<RawItem>], index: usize) -> Vec<Item> {
        let mut items = Vec::with_capacity(arena[index].len());
        for raw in &arena[index] {
            items.push(match raw {
                RawItem::Number(number) => Item::Number(number.clone()),
                RawItem::Text(text) => Item::Text(text.clone()),
                RawItem::List(child) => Item::List(Self::materialize(arena, *child)),
            });
        }
        Self::normalize(&mut items);
        items
    }

    /// Drop trailing null items (a trailing `0`, an empty qualifier, or an empty list), which is
    /// why `1.0.0` equals `1`.
    fn normalize(items: &mut Vec<Item>) {
        let mut i = items.len();
        while i > 0 {
            i -= 1;
            if items[i].is_null() {
                items.remove(i);
            } else if !matches!(items[i], Item::List(_)) {
                break;
            }
        }
    }

    /// Maven's aliases: the explicit release spellings fold into the empty qualifier, and `cr`
    /// is a spelling of `rc`. A single `a`/`b`/`m` immediately followed by a digit is the legacy
    /// `alpha`/`beta`/`milestone` shorthand (`1.0a1` == `1.0-alpha-1`, `1.0.0-M1` < `1.0.0`),
    /// which is why the caller states whether the qualifier ended at a digit.
    fn alias(text: &str, followed_by_digit: bool) -> &str {
        if followed_by_digit && text.len() == 1 {
            match text {
                "a" => return "alpha",
                "b" => return "beta",
                "m" => return "milestone",
                _ => {}
            }
        }
        match text {
            "ga" | "final" | "release" => "",
            "cr" => "rc",
            other => other,
        }
    }

    /// Digits without leading zeros; the empty string is zero.
    fn normalize_number(text: &str) -> String {
        let trimmed = text.trim_start_matches('0');
        if trimmed.is_empty() {
            "0".to_owned()
        } else {
            trimmed.to_owned()
        }
    }

    /// The known-qualifier rank, or `None` for an unknown qualifier (which sorts after `sp`).
    fn qualifier_index(text: &str) -> Option<usize> {
        QUALIFIERS.iter().position(|known| *known == text)
    }

    fn compare_numbers(left: &str, right: &str) -> Ordering {
        left.len().cmp(&right.len()).then_with(|| left.cmp(right))
    }

    fn compare_qualifiers(left: &str, right: &str) -> Ordering {
        match (Self::qualifier_index(left), Self::qualifier_index(right)) {
            (Some(a), Some(b)) => a.cmp(&b),
            (None, None) => left.cmp(right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
        }
    }

    /// Compare an item against an absent one (`Item.compareTo(null)`), which is how a shorter
    /// version is padded on the right.
    fn compare_to_none(item: &Item) -> Ordering {
        match item {
            Item::Number(number) => {
                if number == "0" {
                    Ordering::Equal
                } else {
                    Ordering::Greater
                }
            }
            Item::Text(text) => Self::qualifier_index(text)
                .map_or(Ordering::Greater, |index| index.cmp(&RELEASE_INDEX)),
            Item::List(items) => {
                for item in items {
                    let ordering = Self::compare_to_none(item);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                Ordering::Equal
            }
        }
    }

    fn compare_item(left: &Item, right: &Item) -> Ordering {
        match (left, right) {
            (Item::Number(a), Item::Number(b)) => Self::compare_numbers(a, b),
            (Item::Text(a), Item::Text(b)) => Self::compare_qualifiers(a, b),
            (Item::List(a), Item::List(b)) => Self::compare_items(a, b),
            // Maven's cross-kind rules: a number outranks a qualifier and a list; a qualifier
            // outranks neither. A list outranks a qualifier — `ListItem.compareTo(StringItem)`
            // is 1, as in `1-1 > 1-sp` — but still ranks below a number.
            (Item::Number(_), Item::Text(_) | Item::List(_)) | (Item::List(_), Item::Text(_)) => {
                Ordering::Greater
            }
            (Item::Text(_), Item::Number(_) | Item::List(_)) | (Item::List(_), Item::Number(_)) => {
                Ordering::Less
            }
        }
    }

    fn compare_items(left: &[Item], right: &[Item]) -> Ordering {
        let mut left = left.iter();
        let mut right = right.iter();
        loop {
            match (left.next(), right.next()) {
                (None, None) => return Ordering::Equal,
                (Some(a), Some(b)) => {
                    let ordering = Self::compare_item(a, b);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                (Some(a), None) => {
                    let ordering = Self::compare_to_none(a);
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
                (None, Some(b)) => {
                    let ordering = Self::compare_to_none(b).reverse();
                    if ordering != Ordering::Equal {
                        return ordering;
                    }
                }
            }
        }
    }

    /// A canonical spelling of the comparison state, so `Hash` agrees with `Eq`
    /// (`1.0.0` and `1` hash identically).
    fn canonical_slice(items: &[Item], out: &mut String) {
        for item in items {
            match item {
                Item::Number(number) => {
                    let _ = write!(out, "n{number};");
                }
                Item::Text(text) => {
                    let _ = write!(out, "s{}:{text};", text.len());
                }
                Item::List(inner) => {
                    out.push('[');
                    Self::canonical_slice(inner, out);
                    out.push(']');
                }
            }
        }
    }
}

impl Item {
    fn is_null(&self) -> bool {
        match self {
            Self::Number(number) => number == "0",
            Self::Text(text) => Version::qualifier_index(text) == Some(RELEASE_INDEX),
            Self::List(items) => items.is_empty(),
        }
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        Self::compare_items(&self.items, &other.items) == Ordering::Equal
    }
}

impl Eq for Version {}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        Self::compare_items(&self.items, &other.items)
    }
}

impl Hash for Version {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let mut canonical = String::new();
        Self::canonical_slice(&self.items, &mut canonical);
        canonical.hash(state);
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// One bound of a Maven version range.
#[derive(Debug, Clone)]
struct VersionBound {
    version: Version,
    inclusive: bool,
}

/// One Maven range: `[1.0,2.0)`, `(,1.0]`, `[1.5,)`, or `[1.0]`.
#[derive(Debug, Clone)]
struct VersionRange {
    lower: Option<VersionBound>,
    upper: Option<VersionBound>,
}

impl VersionRange {
    fn matches(&self, version: &Version) -> bool {
        if let Some(lower) = &self.lower {
            let ordering = version.cmp(&lower.version);
            match (ordering, lower.inclusive) {
                (Ordering::Less, _) | (Ordering::Equal, false) => return false,
                _ => {}
            }
        }
        if let Some(upper) = &self.upper {
            let ordering = version.cmp(&upper.version);
            match (ordering, upper.inclusive) {
                (Ordering::Greater, _) | (Ordering::Equal, false) => return false,
                _ => {}
            }
        }
        true
    }
}

/// The operator a requirement was written with.
#[derive(Debug, Clone)]
enum ReqKind {
    /// `*` — every version.
    Any,
    /// A bare version or `^1.2.3` — compatible with the base (Cargo's rule).
    Caret(Version),
    /// `~1.2.3` — the same major and minor.
    Tilde(Version),
    /// `=1.2.3` — exactly the base.
    Exact(Version),
    /// `1.*`, `1.2.*`.
    Wildcard {
        major: Option<String>,
        minor: Option<String>,
    },
    /// Maven range syntax, possibly a union of ranges.
    Ranges(Vec<VersionRange>),
}

/// A version requirement, as written in a manifest.
///
/// A bare version is a **caret** requirement even though the registry is Maven: `2.0.16` means
/// `>=2.0.16, <3.0.0` (`0.2.3` means `<0.3.0`, `0.0.3` means `<0.0.4`, Cargo's zero rules).
/// Maven's own range syntax is accepted verbatim and keeps Maven semantics, so a project that
/// wants an exact repository-style pin can write `[2.0.16]` or `=2.0.16`.
#[derive(Debug, Clone)]
pub struct VersionReq {
    raw: String,
    kind: ReqKind,
}

impl VersionReq {
    /// Parse a requirement.
    ///
    /// # Errors
    /// [`VersionError::Empty`] for a blank string, [`VersionError::Whitespace`] for embedded
    /// whitespace, and [`VersionError::Requirement`] for a malformed operator or range.
    pub fn parse(raw: &str) -> Result<Self, VersionError> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(VersionError::Empty {
                raw: raw.to_owned(),
            });
        }
        if raw.chars().any(char::is_whitespace) {
            return Err(VersionError::Whitespace {
                raw: raw.to_owned(),
            });
        }
        let kind = if trimmed.starts_with('[') || trimmed.starts_with('(') {
            ReqKind::Ranges(Self::parse_ranges(trimmed, raw)?)
        } else if trimmed == "*" {
            ReqKind::Any
        } else if trimmed.contains([',', '[', ']', '(', ')']) {
            // A range that did not start with a bracket, or a comma-separated list: neither is
            // a version, and silently parsing it as a weird bare version would turn a typo into
            // an unsatisfiable pin.
            return Err(VersionError::Requirement {
                raw: raw.to_owned(),
                reason: RequirementError::MalformedRange,
            });
        } else if let Some(rest) = trimmed.strip_prefix('=') {
            ReqKind::Exact(Self::version(rest, raw)?)
        } else if let Some(rest) = trimmed.strip_prefix('~') {
            ReqKind::Tilde(Self::version(rest, raw)?)
        } else if let Some(rest) = trimmed.strip_prefix('^') {
            ReqKind::Caret(Self::version(rest, raw)?)
        } else if trimmed.contains('*')
            || trimmed
                .split('.')
                .any(|part| part.eq_ignore_ascii_case("x"))
        {
            Self::parse_wildcard(trimmed, raw)?
        } else {
            ReqKind::Caret(Self::version(trimmed, raw)?)
        };
        Ok(Self {
            raw: raw.to_owned(),
            kind,
        })
    }

    /// The requirement as it was written.
    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The one version this requirement pins to, when it pins one.
    ///
    /// A provider that must answer when its version index is unavailable (a Maven mirror without
    /// `maven-metadata.xml`) can offer exactly this version: `=1.2.3`, `^1.2.3`, `~1.2.3` and a
    /// single-version range `[1.2.3]` all name a concrete base, while `1.*`/`*`/multi-version
    /// ranges do not.
    pub fn pinned_base(&self) -> Option<&Version> {
        match &self.kind {
            ReqKind::Exact(base) | ReqKind::Caret(base) | ReqKind::Tilde(base) => Some(base),
            ReqKind::Ranges(ranges) if ranges.len() == 1 => {
                let range = &ranges[0];
                match (&range.lower, &range.upper) {
                    (Some(lower), Some(upper))
                        if lower.inclusive && upper.inclusive && lower.version == upper.version =>
                    {
                        Some(&lower.version)
                    }
                    _ => None,
                }
            }
            ReqKind::Any | ReqKind::Wildcard { .. } | ReqKind::Ranges(_) => None,
        }
    }

    /// Whether `version` satisfies this requirement.
    pub fn matches(&self, version: &Version) -> bool {
        match &self.kind {
            ReqKind::Any => true,
            ReqKind::Exact(base) => version == base,
            ReqKind::Caret(base) => version >= base && version < &Self::next_compatible(base),
            ReqKind::Tilde(base) => {
                version >= base && Self::next_tilde(base).is_none_or(|upper| version < &upper)
            }
            ReqKind::Wildcard { major, minor } => match (major, minor) {
                (None, _) => true,
                (Some(major), None) => {
                    let lower = Self::numeric(major, "0", "0");
                    let upper = Self::numeric(&Self::bump(major), "0", "0");
                    version >= &lower && version < &upper
                }
                (Some(major), Some(minor)) => {
                    let lower = Self::numeric(major, minor, "0");
                    let upper = Self::numeric(major, &Self::bump(minor), "0");
                    version >= &lower && version < &upper
                }
            },
            ReqKind::Ranges(ranges) => ranges.iter().any(|range| range.matches(version)),
        }
    }

    /// The upper bound of a caret requirement, from the base's leading numeric components.
    ///
    /// Cargo's zero rules: `0.0.3` is compatible up to `0.0.4`, `0.0` up to `0.1.0`, `0.2.3` up
    /// to `0.3.0`, and `1.2.3` up to `2.0.0`.
    fn next_compatible(base: &Version) -> Version {
        let components = base.numeric_components();
        let major = components.first().map_or("0", String::as_str);
        let minor = components.get(1).map_or("0", String::as_str);
        let patch = components.get(2).map_or("0", String::as_str);
        let has_minor = components.len() > 1;
        let has_patch = components.len() > 2;
        if major != "0" {
            Self::numeric(&Self::bump(major), "0", "0")
        } else if has_minor && minor != "0" {
            Self::numeric("0", &Self::bump(minor), "0")
        } else if has_patch {
            Self::numeric("0", "0", &Self::bump(patch))
        } else if has_minor {
            Self::numeric("0", "1", "0")
        } else {
            Self::numeric("1", "0", "0")
        }
    }

    /// The upper bound of a tilde requirement: the same major and minor, so `~1` can reach
    /// `1.x` and `~1.2` stops at `1.3.0`.
    fn next_tilde(base: &Version) -> Option<Version> {
        let components = base.numeric_components();
        match components.as_slice() {
            [major] => Some(Self::numeric(&Self::bump(major), "0", "0")),
            [major, minor, ..] => Some(Self::numeric(major, &Self::bump(minor), "0")),
            [] => None,
        }
    }

    /// Add one to a normalized decimal component, at any width.
    fn bump(component: &str) -> String {
        let mut digits: Vec<u8> = component.bytes().map(|byte| byte - b'0').collect();
        let mut index = digits.len();
        loop {
            if index == 0 {
                digits.insert(0, 1);
                break;
            }
            index -= 1;
            if digits[index] == 9 {
                digits[index] = 0;
            } else {
                digits[index] += 1;
                break;
            }
        }
        digits
            .into_iter()
            .map(|digit| char::from(b'0' + digit))
            .collect()
    }

    /// A parsed numeric version for a derived bound; never fails because the spelling is built
    /// from digits.
    fn numeric(major: &str, minor: &str, patch: &str) -> Version {
        Version::parse(&format!("{major}.{minor}.{patch}"))
            .expect("a three-component numeric version parses")
    }

    /// `1.*` / `1.2.*` / `1.x`.
    fn parse_wildcard(trimmed: &str, raw: &str) -> Result<ReqKind, VersionError> {
        let parts: Vec<&str> = trimmed.split('.').collect();
        let malformed = || VersionError::Requirement {
            raw: raw.to_owned(),
            reason: RequirementError::WildcardComponents,
        };
        if parts.is_empty() || parts.len() > 3 {
            return Err(malformed());
        }
        let is_wildcard = |part: &str| part == "*" || part.eq_ignore_ascii_case("x");
        let number = |part: &str| -> Result<String, VersionError> {
            if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) {
                Ok(Version::normalize_number(part))
            } else {
                Err(malformed())
            }
        };
        let major = match parts[0] {
            part if is_wildcard(part) => None,
            part => Some(number(part)?),
        };
        let minor = match parts.get(1) {
            None => None,
            Some(part) if is_wildcard(part) => None,
            Some(part) => Some(number(part)?),
        };
        if let Some(third) = parts.get(2)
            && !is_wildcard(third)
        {
            return Err(malformed());
        }
        Ok(ReqKind::Wildcard { major, minor })
    }

    /// Parse the Maven range-set syntax, including unions (`[1.0,2.0),[3.0,)`).
    fn parse_ranges(trimmed: &str, raw: &str) -> Result<Vec<VersionRange>, VersionError> {
        let malformed = || VersionError::Requirement {
            raw: raw.to_owned(),
            reason: RequirementError::MalformedRange,
        };
        let chars: Vec<char> = trimmed.chars().collect();
        let mut ranges = Vec::new();
        let mut i = 0usize;
        loop {
            while i < chars.len() && chars[i] == ' ' {
                i += 1;
            }
            if i >= chars.len() {
                break;
            }
            let opening = chars[i];
            if opening != '[' && opening != '(' {
                return Err(malformed());
            }
            i += 1;
            let content_start = i;
            while i < chars.len() && chars[i] != ']' && chars[i] != ')' {
                i += 1;
            }
            if i >= chars.len() {
                return Err(malformed());
            }
            let closing = chars[i];
            let content: String = chars[content_start..i].iter().collect();
            i += 1;
            ranges.push(Self::parse_range_content(
                &content,
                opening == '[',
                closing == ']',
                raw,
            )?);
            if i < chars.len() {
                if chars[i] != ',' {
                    return Err(malformed());
                }
                i += 1;
            }
        }
        if ranges.is_empty() {
            return Err(malformed());
        }
        Ok(ranges)
    }

    /// The inside of one `[...]`/`(...)`: a single version or two bounds. `lower_inclusive`
    /// (the opening `[`) and `upper_inclusive` (the closing `]`) are the range's own signs.
    fn parse_range_content(
        content: &str,
        lower_inclusive: bool,
        upper_inclusive: bool,
        raw: &str,
    ) -> Result<VersionRange, VersionError> {
        let malformed = || VersionError::Requirement {
            raw: raw.to_owned(),
            reason: RequirementError::MalformedRange,
        };
        if content.is_empty() {
            return Err(malformed());
        }
        if !content.contains(',') {
            let version = Version::parse(content).map_err(|_| malformed())?;
            return Ok(VersionRange {
                lower: Some(VersionBound {
                    version: version.clone(),
                    inclusive: true,
                }),
                upper: Some(VersionBound {
                    version,
                    inclusive: true,
                }),
            });
        }
        let mut parts = content.splitn(2, ',');
        let lower_text = parts.next().unwrap_or("").trim();
        let upper_text = parts.next().unwrap_or("").trim();
        if upper_text.contains(',') {
            return Err(malformed());
        }
        let bound = |text: &str| -> Result<Option<VersionBound>, VersionError> {
            if text.is_empty() {
                return Ok(None);
            }
            let version = Version::parse(text).map_err(|_| malformed())?;
            Ok(Some(VersionBound {
                version,
                inclusive: true,
            }))
        };
        let lower = bound(lower_text)?.map(|bound| VersionBound {
            version: bound.version,
            inclusive: lower_inclusive,
        });
        let upper = bound(upper_text)?.map(|bound| VersionBound {
            version: bound.version,
            inclusive: upper_inclusive,
        });
        Ok(VersionRange { lower, upper })
    }

    /// Parse an operand version, reporting the requirement spelling on failure.
    fn version(text: &str, raw: &str) -> Result<Version, VersionError> {
        Version::parse(text).map_err(|_| VersionError::Requirement {
            raw: raw.to_owned(),
            reason: RequirementError::EmptyOperand,
        })
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Requirements compare by their written spelling. Two spellings of one requirement (`1.0` and
/// `^1.0`) are deliberately not equal here: the spelling is what diagnostics show and what a
/// consumer folds into a key, and normalizing it would make one requirement print as another.
impl PartialEq for VersionReq {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl Eq for VersionReq {}

impl Hash for VersionReq {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl FromStr for VersionReq {
    type Err = VersionError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        Self::parse(raw)
    }
}

/// A version or requirement that could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// The value was empty or only whitespace.
    Empty {
        /// The offending value.
        raw: String,
    },
    /// The value contained whitespace.
    Whitespace {
        /// The offending value.
        raw: String,
    },
    /// A requirement's operator or range was malformed.
    Requirement {
        /// The offending value.
        raw: String,
        /// What was wrong with it.
        reason: RequirementError,
    },
}

/// Why a requirement was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequirementError {
    /// An operator had no operand (`=`, `~`, `^`).
    EmptyOperand,
    /// A range was missing a bracket, had too many bounds, or named no range at all.
    MalformedRange,
    /// A wildcard named more than three components or mixed a wildcard with a suffix.
    WildcardComponents,
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { raw } => write!(f, "`{raw}` is an empty version"),
            Self::Whitespace { raw } => write!(f, "`{raw}` contains whitespace"),
            Self::Requirement { raw, reason } => match reason {
                RequirementError::EmptyOperand => {
                    write!(
                        f,
                        "`{raw}` names a version requirement operator with no operand"
                    )
                }
                RequirementError::MalformedRange => write!(
                    f,
                    "`{raw}` is not a version or a Maven range (`[1.0,2.0)`, `(,1.0]`, `[1.5,)`)"
                ),
                RequirementError::WildcardComponents => write!(
                    f,
                    "`{raw}` is not a wildcard requirement (`1.*`, `1.2.*`, `*`)"
                ),
            },
        }
    }
}

impl core::error::Error for VersionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString as _;

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn req(text: &str) -> VersionReq {
        VersionReq::parse(text).unwrap()
    }

    #[test]
    fn maven_ordering_follows_comparable_version() {
        let ordered = [
            "1.0-alpha-1",
            "1.0-alpha-2",
            "1.0-beta-1",
            "1.0-milestone-1",
            "1.0-rc1",
            "1.0-snapshot",
            "1.0",
            "1.0-sp",
            "1.0-abc",
            "1.0.1",
            "1.1",
            "2.0",
        ];
        for window in ordered.windows(2) {
            let left = version(window[0]);
            let right = version(window[1]);
            assert!(left < right, "expected `{}` < `{}`", window[0], window[1]);
        }
    }

    #[test]
    fn maven_ordering_treats_trailing_zeros_as_absent() {
        assert_eq!(version("1"), version("1.0"));
        assert_eq!(version("1"), version("1.0.0"));
        assert_eq!(version("1.0.0-ga"), version("1"));
        assert_eq!(version("1.0.0-final"), version("1"));
        assert_eq!(version("1.0.0-release"), version("1"));
        assert_eq!(version("1.0-cr1"), version("1.0-rc1"));
    }

    #[test]
    fn digit_letter_transitions_parse_as_nested_lists() {
        assert!(version("1.0alpha1") < version("1.0alpha2"));
        assert!(version("1.0alpha2") < version("1.0beta1"));
        assert_eq!(version("1.0alpha1"), version("1.0-alpha-1"));
        assert!(version("1.0-1") < version("1.0.1"));
        assert!(version("1-1") < version("1.0.1"));
    }

    #[test]
    fn a_single_letter_before_a_digit_aliases_like_maven() {
        assert_eq!(version("1.0a1"), version("1.0-alpha-1"));
        assert_eq!(version("1.0b1"), version("1.0-beta-1"));
        assert_eq!(version("1.0m1"), version("1.0-milestone-1"));
        // A milestone is a pre-release: it must sort below its release, or a caret
        // requirement admits it as a newer satisfying version. Both the dash and the legacy
        // one-letter spellings are used in the wild (`1.0.0-M1`, `1.0.0.M1`).
        assert!(version("1.0.0-M1") < version("1.0.0"));
        assert!(version("1.0.0.M1") < version("1.0.0"));
        assert!(version("6.0.0-M1") < version("6.0.0"));
        assert!(!req("6.0.0").matches(&version("6.0.0-M1")));
    }

    #[test]
    fn a_list_sorts_after_a_string_item_at_the_same_level() {
        // Maven's `ListItem.compareTo(StringItem)` is 1, as in `1-1 > 1-sp`.
        assert!(version("1-1") > version("1-sp"));
        assert!(version("1-rc1") > version("1-rc.alpha"));
    }

    #[test]
    fn qualifiers_compare_case_insensitively() {
        assert_eq!(version("1.0-RC1"), version("1.0-rc1"));
        assert_eq!(version("1.0-Beta"), version("1.0-beta"));
    }

    #[test]
    fn huge_numeric_components_compare_by_length_then_digits() {
        assert!(version("1.99999999999999999999") > version("1.9999999999999999999"));
        assert!(version("1.100000000000000000000") < version("1.100000000000000000001"));
    }

    #[test]
    fn equality_agrees_with_hash() {
        use std::collections::hash_map::DefaultHasher;
        let hash = |v: &Version| {
            let mut hasher = DefaultHasher::new();
            v.hash(&mut hasher);
            hasher.finish()
        };
        assert_eq!(hash(&version("1.0.0")), hash(&version("1")));
        assert_ne!(hash(&version("1.0")), hash(&version("1.0.1")));
    }

    #[test]
    fn a_bare_version_is_a_caret_requirement() {
        let requirement = req("2.0.16");
        assert!(requirement.matches(&version("2.0.16")));
        assert!(requirement.matches(&version("2.9.9")));
        assert!(requirement.matches(&version("2.1.0-jre")));
        assert!(!requirement.matches(&version("1.9.9")));
        assert!(!requirement.matches(&version("3.0.0")));
    }

    #[test]
    fn caret_follows_cargos_zero_rules() {
        assert!(req("0.0.3").matches(&version("0.0.3")));
        assert!(!req("0.0.3").matches(&version("0.0.4")));
        assert!(req("0.2.3").matches(&version("0.2.9")));
        assert!(!req("0.2.3").matches(&version("0.3.0")));
        assert!(req("0.0").matches(&version("0.0.9")));
        assert!(!req("0.0").matches(&version("0.1.0")));
        assert!(req("0").matches(&version("0.9.0")));
        assert!(!req("0").matches(&version("1.0.0")));
    }

    #[test]
    fn operators_and_wildcards_parse() {
        assert!(req("=1.2.3").matches(&version("1.2.3")));
        assert!(!req("=1.2.3").matches(&version("1.2.4")));
        assert!(req("~1.2.3").matches(&version("1.2.9")));
        assert!(!req("~1.2.3").matches(&version("1.3.0")));
        assert!(req("~1").matches(&version("1.9.0")));
        assert!(!req("~1").matches(&version("2.0.0")));
        assert!(req("1.*").matches(&version("1.9.9")));
        assert!(!req("1.*").matches(&version("2.0.0")));
        assert!(req("1.2.x").matches(&version("1.2.9")));
        assert!(!req("1.2.x").matches(&version("1.3.0")));
        assert!(req("*").matches(&version("0.0.1-alpha")));
    }

    #[test]
    fn maven_ranges_keep_maven_semantics() {
        assert!(req("[1.0]").matches(&version("1.0")));
        assert!(!req("[1.0]").matches(&version("1.0.1")));
        assert!(req("[1.0,2.0)").matches(&version("1.0")));
        assert!(req("[1.0,2.0)").matches(&version("1.9.9")));
        assert!(!req("[1.0,2.0)").matches(&version("2.0")));
        assert!(req("(1.0,2.0]").matches(&version("2.0")));
        assert!(!req("(1.0,2.0]").matches(&version("1.0")));
        assert!(req("(,1.0]").matches(&version("0.9")));
        assert!(req("[1.5,)").matches(&version("99.0")));
        assert!(!req("[1.5,)").matches(&version("1.4")));
    }

    #[test]
    fn maven_range_sets_union() {
        let requirement = req("[1.0,2.0),[3.0,4.0)");
        assert!(requirement.matches(&version("1.5")));
        assert!(!requirement.matches(&version("2.5")));
        assert!(requirement.matches(&version("3.0")));
        assert!(!requirement.matches(&version("4.0")));
    }

    #[test]
    fn malformed_requirements_are_rejected() {
        assert!(VersionReq::parse("").is_err());
        assert!(VersionReq::parse("1.0,2.0").is_err());
        assert!(VersionReq::parse("[1.0,2.0,3.0)").is_err());
        assert!(VersionReq::parse("[1.0,2.0").is_err());
        assert!(VersionReq::parse("1.2.3.4.*").is_err());
        assert!(VersionReq::parse("= ").is_err());
        assert!(Version::parse("1 .0").is_err());
    }

    #[test]
    fn display_keeps_the_written_spelling() {
        assert_eq!(req("[1.0,2.0)").to_string(), "[1.0,2.0)");
        assert_eq!(version("1.0.0-RELEASE").to_string(), "1.0.0-RELEASE");
    }

    #[test]
    fn huge_versions_do_not_overflow() {
        assert!(version("9999999999999999999999.0") > version("2.0"));
        assert!(req("9999999999999999999999.0").matches(&version("9999999999999999999999.5")));
    }
}
