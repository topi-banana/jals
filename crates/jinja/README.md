# `jinja`

A small [Jinja2] template engine with [minijinja]'s API, no dependencies, and `no_std + alloc` in
every configuration.

```rust
use jinja::{Environment, Error, context};

fn main() -> Result<(), Error> {
    let mut env = Environment::new();
    env.add_template("greeting", "Hello, {{ name }}!")?;

    let rendered = env.get_template("greeting")?.render(context! { name => "world" })?;
    assert_eq!(rendered, "Hello, world!");
    Ok(())
}
```

## What it has

`{{ … }}` writes a value, `{% … %}` is a directive, `{# … #}` is a comment. The directives are
`if` / `elif` / `else` / `endif`, `for` / `endfor`, and `include`. An expression has names, `.field`
and `["field"]` access, string / integer / float / `true` / `false` / `none` literals, `not`, `and`,
`or`, the six comparisons, parentheses, and `|` filters with arguments. A `{% for %}` binds `loop`
with `index`, `index0`, `revindex`, `revindex0`, `first`, `last` and `length`.

Eleven filters are registered by `Environment::new`, under twelve names: `default`, `upper`, `lower`, `trim`, `string`,
`length` (`count`), `join`, `replace`, `first`, `last` and `reverse`. `Environment::empty` registers
none, and `Environment::add_filter` takes any `Fn(&Value, &[Value]) -> Result<Value, Error>`.

## What it deliberately does not

`{% extends %}`, `{% block %}`, `{% macro %}`, `{% set %}`, `is` tests, auto-escaping, arithmetic,
and serde. Each is a second language inside the template, and this engine is meant for documents
whose author is also the author of the program rendering them — a config file, a manifest, a
generated header. Reach for [minijinja] where a template is written by somebody else.

Leaving serde out is what lets the crate have no dependencies at all. A `Value` is built from its
`From` impls, from `context!`, or from an `Object` a consumer implements.

## Where it differs from minijinja on purpose

| | Here | minijinja |
| --- | --- | --- |
| A lookup that finds nothing | `None` — distinct from a key holding undefined | undefined |
| `Object` | `!Send`, `&str` keys | `Send + Sync`, `Value` keys |
| `{{ a_map }}` | an error: a collection has no text form | a debug rendering |
| Whitespace control | one `set_trim_block_lines`; no `{%- -%}` | `trim_blocks` + `lstrip_blocks` + `{%- -%}` |
| A filter's arguments | its subject and its arguments, nothing else | plus the render `State` |
| A name nothing defines | `set_strict_variables` decides — error or undefined | undefined |
| `a and b` / `a or b` | the answer as a boolean | the operand that decided |
| `{{ 1.0 }}` | `1` — Rust's `Display`, never exponential | `1.0` |

The first row is the load-bearing one. Keeping *there is no such key* apart from *this key holds
nothing* is what lets `Environment::set_strict_variables` refuse a **typo** while `| default(…)`
still answers for a value the author knows may be missing — two different mistakes with two
different fixes.

## Making a value out of your own type

`Object` is the seam a domain rule lives behind. A set that answers membership for *any* name,
rather than only for the names it holds, is a rule about the consumer's domain and stays in the
consumer's crate:

```rust
use std::collections::BTreeSet;

use jinja::{Enumerator, Object, Value};

#[derive(Debug)]
struct Features(BTreeSet<String>);

impl Object for Features {
    fn get_value(&self, key: &str) -> Option<Value> {
        // Every name is a well-formed question, so this never answers `None`.
        Some(Value::from(self.0.contains(key)))
    }

    fn enumerate(&self) -> Enumerator {
        Enumerator::Values(self.0.iter().map(|name| Value::from(name.as_str())).collect())
    }
}
```

## Undefined behaviour

`Lenient`, `SemiStrict` and `Strict` are a ladder — each refuses everything the one before it
refuses and one thing more. `Chainable` is a step aside rather than a rung: it is `Lenient` with the
one thing `Lenient` refuses allowed. The table is the whole rule:

| | `{{ unset }}` | `{{ unset.field }}` | `{{ unset \| default("x") }}` |
| --- | --- | --- | --- |
| `Lenient` (default) | `""` | error | `x` |
| `Chainable` | `""` | undefined | `x` |
| `SemiStrict` | error | error | `x` |
| `Strict` | error | error | error |

`SemiStrict` is the rung a tool that writes files wants: an unset value reaching the output is the
silent wrong answer, and the author who meant it says so with a `default`.

`| default` with **no** argument names no fallback, so it leaves the value unset rather than
emptying it — `Lenient` still writes `""` for one, and a stricter rung still gets its say. Reaching
for the empty string is `| default("")`, written out.

[Jinja2]: https://jinja.palletsprojects.com
[minijinja]: https://docs.rs/minijinja
