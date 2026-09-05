//! What the engine promises, asserted from outside the crate.
//!
//! These are the tests that used to live beside `jals-project`'s resource templating and moved here
//! with the engine. What stayed there is what is still that crate's own: which resources are
//! declared, and what a render sees.

use std::collections::{BTreeMap, BTreeSet};

use jinja::{Enumerator, Environment, Error, ErrorKind, Object, UndefinedBehavior, Value, context};

/// A set that answers *membership* for any name at all, which is the shape a consumer's domain rule
/// takes: `features.anything` is a well-formed question whether or not anything declared it.
#[derive(Debug)]
struct Membership(BTreeSet<String>);

impl Membership {
    fn of(names: &[&str]) -> Value {
        Value::from_object(Self(names.iter().map(|name| (*name).to_owned()).collect()))
    }
}

impl Object for Membership {
    fn get_value(&self, key: &str) -> Option<Value> {
        Some(Value::from(self.0.contains(key)))
    }

    fn enumerate(&self) -> Enumerator {
        Enumerator::Values(
            self.0
                .iter()
                .map(|name| Value::from(name.as_str()))
                .collect(),
        )
    }
}

/// The configuration a tool that writes files wants: an unset value never reaches the output, a
/// name nobody defined is a typo, and a directive alone on its line leaves no blank line behind.
fn strict() -> Environment {
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::SemiStrict);
    env.set_strict_variables(true);
    env.set_trim_block_lines(true);
    env
}

/// `package` with both keys set, and `features` answering for the given names.
fn context(version: Option<&str>, features: &[&str]) -> Value {
    let mut package = BTreeMap::new();
    package.insert("name".to_owned(), Value::from("hellomod"));
    package.insert(
        "version".to_owned(),
        version.map_or(Value::UNDEFINED, Value::from),
    );
    context! { package => Value::from(package), features => Membership::of(features) }
}

fn render(source: &str, context: Value) -> Result<String, Error> {
    strict().render_str(source, context)
}

fn message(source: &str, context: Value) -> String {
    render(source, context)
        .expect_err("this template fails")
        .to_string()
}

#[test]
fn text_with_no_tags_renders_to_itself() {
    // The property a byte-for-byte pass-through rests on: a document with no tags is not merely
    // copied by some other code path, it is text this engine leaves alone.
    for source in [
        "{\n  \"a\": { \"b\": 1 }\n}\n",
        "<?xml version=\"1.0\"?>\n<config><item/></config>\n",
        "\u{feff}{ \"bom\": true }\n",
        "a\r\nb\r\n",
        "{ } { {  }",
        "",
    ] {
        assert_eq!(
            render(source, context(Some("0.1.0"), &[])).as_deref(),
            Ok(source),
            "{source:?}"
        );
    }
}

#[test]
fn a_value_renders_and_an_unset_one_needs_a_default() {
    let full = context(Some("0.1.0"), &[]);
    assert_eq!(
        render("{{ package.name }}-{{ package.version }}", full.clone()).as_deref(),
        Ok("hellomod-0.1.0")
    );
    // Both spellings reach the same value.
    assert_eq!(
        render("{{ package[\"name\"] }}", full).as_deref(),
        Ok("hellomod")
    );

    // A key that is declared but unset is known and absent: writing it is an error, testing it is
    // false, and `default` is how the intentional case is spelled.
    let bare = || context(None, &[]);
    let error = render("{{ package.version }}", bare()).expect_err("the value is not set");
    assert_eq!(error.kind(), ErrorKind::UndefinedError);
    assert_eq!(
        error.to_string(),
        "line 1, column 1: this value is not set; write `| default(\"…\")` to say what to use \
         instead"
    );
    assert_eq!(
        render("{{ package.version | default(\"0.0.0\") }}", bare()).as_deref(),
        Ok("0.0.0")
    );
    assert_eq!(
        render(
            "{% if package.version %}set{% else %}unset{% endif %}",
            bare()
        )
        .as_deref(),
        Ok("unset")
    );
}

#[test]
fn an_unknown_name_is_an_error_that_says_what_the_known_ones_are() {
    let full = || context(Some("0.1.0"), &[]);

    let error = message("{{ package.licence }}", full());
    // A field typo gets the same help a root typo gets: the names that are there.
    assert_eq!(
        error,
        "line 1, column 1: `package` has no field `licence`; it has `name` and `version`"
    );

    // The fix for a typo is a name, so the error hands the names over rather than only refusing.
    let error = render("a\nb\n  {{ nope }}\n", full()).expect_err("no such name");
    assert_eq!(error.kind(), ErrorKind::UnknownVariable);
    assert_eq!(error.line(), Some(3));
    assert_eq!(error.column(), Some(3));
    assert_eq!(
        error.to_string(),
        "line 3, column 3: unknown name `nope`; a template can read `features` and `package`"
    );

    // A namespace has no text form.
    assert_eq!(
        message("{{ package }}", full()),
        "line 1, column 1: a map has no text form, so it cannot be written"
    );

    // Off — Jinja's own answer — the same names are undefined rather than refused, which the
    // undefined behaviour then decides the fate of.
    let mut lenient = Environment::new();
    lenient.set_trim_block_lines(true);
    assert_eq!(
        lenient.render_str("[{{ nope }}]", full()).as_deref(),
        Ok("[]")
    );
}

#[test]
fn an_object_answers_for_any_name_it_chooses_to() {
    let context = || context(Some("0.1.0"), &["server", "1.20.1", "mixin-extras"]);
    assert_eq!(
        render("{{ features.server }}", context()).as_deref(),
        Ok("true")
    );
    // A name the set does not hold is `false`, never an error: the object answered, so there is no
    // unknown name for `strict_variables` to refuse.
    assert_eq!(
        render("{{ features.client }}", context()).as_deref(),
        Ok("false")
    );
    // The bracket spelling is not sugar: neither `1.20.1` nor `mixin-extras` is a name `a.b` can
    // carry.
    assert_eq!(
        render(
            "{{ features[\"1.20.1\"] }} {{ features[\"mixin-extras\"] }}",
            context()
        )
        .as_deref(),
        Ok("true true")
    );
    assert_eq!(
        render(
            "{% if features[\"1.21\"] %}y{% else %}n{% endif %}",
            context()
        )
        .as_deref(),
        Ok("n")
    );
}

#[test]
fn conditionals_take_exactly_one_arm() {
    let source = "{% if features.a %}A{% elif features.b %}B{% else %}C{% endif %}";
    assert_eq!(render(source, context(None, &["b"])).as_deref(), Ok("B"));
    assert_eq!(
        render(source, context(None, &["a", "b"])).as_deref(),
        Ok("A")
    );
    assert_eq!(render(source, context(None, &[])).as_deref(), Ok("C"));

    let only_a = || context(None, &["a"]);
    assert_eq!(
        render(
            "{% if not features.b and features.a %}y{% endif %}",
            only_a()
        )
        .as_deref(),
        Ok("y")
    );
    assert_eq!(
        render(
            "{% if (features.b or features.a) and not features.c %}y{% endif %}",
            only_a()
        )
        .as_deref(),
        Ok("y")
    );
}

#[test]
fn comparisons_answer_within_a_shape_and_refuse_across_two() {
    let context = || context(Some("2.0"), &[]);
    assert_eq!(
        render("{% if package.version == \"2.0\" %}y{% endif %}", context()).as_deref(),
        Ok("y")
    );
    assert_eq!(
        render("{% if 3 > 2 and 2 <= 2 %}y{% endif %}", context()).as_deref(),
        Ok("y")
    );
    assert_eq!(
        render("{{ package.name != \"other\" }}", context()).as_deref(),
        Ok("true")
    );
    // Ordering across two shapes answers nothing rather than answering wrongly.
    assert_eq!(
        message("{% if package.name < 3 %}y{% endif %}", context()),
        "line 1, column 1: a string and a number cannot be compared with `<`"
    );
    // Equality across two shapes is well-formed, and false.
    assert_eq!(
        render("{{ package.name == 3 }}", context()).as_deref(),
        Ok("false")
    );
}

#[test]
fn a_block_tag_alone_on_its_line_takes_the_line_with_it() {
    // Without this rule every `{% if %}` in a JSON document leaves a blank line behind, so the
    // rendered file differs from a hand-written one by whitespace nobody asked for.
    let source =
        "{\n{% if features.server %}\n  \"env\": \"server\",\n{% endif %}\n  \"x\": 1\n}\n";
    assert_eq!(
        render(source, context(None, &["server"])).as_deref(),
        Ok("{\n  \"env\": \"server\",\n  \"x\": 1\n}\n")
    );
    assert_eq!(
        render(source, context(None, &[])).as_deref(),
        Ok("{\n  \"x\": 1\n}\n")
    );

    // Sharing a line with anything at all switches the rule off, so text around a tag is never
    // eaten by surprise.
    assert_eq!(
        render(
            "a {% if features.server %}b{% endif %} c",
            context(None, &["server"])
        )
        .as_deref(),
        Ok("a b c")
    );

    // A comment is a block tag for this purpose, and disappears either way.
    assert_eq!(
        render("a\n{# gone #}\nb\n", context(None, &[])).as_deref(),
        Ok("a\nb\n")
    );
    assert_eq!(
        render("a {# gone #} b", context(None, &[])).as_deref(),
        Ok("a  b")
    );

    // Off by default, which is what Jinja does with no `trim_blocks`/`lstrip_blocks`: the line the
    // tag sat on is still there.
    assert_eq!(
        Environment::new()
            .render_str("a\n{# gone #}\nb\n", context(None, &[]))
            .as_deref(),
        Ok("a\n\nb\n")
    );
}

#[test]
fn a_string_literal_is_how_a_delimiter_is_written() {
    let context = || context(None, &[]);
    assert_eq!(render("{{ \"{{\" }}", context()).as_deref(), Ok("{{"));
    // The closing scan has to know about strings, or this tag ends inside the literal.
    assert_eq!(render("{{ \"}}\" }}", context()).as_deref(), Ok("}}"));
    assert_eq!(
        render("{{ \"{%\" }}{{ \"%}\" }}", context()).as_deref(),
        Ok("{%%}")
    );
    assert_eq!(
        render("{{ \"a\\\"b\\\\c\" }}", context()).as_deref(),
        Ok("a\"b\\c")
    );
}

#[test]
fn a_loop_walks_its_source_and_binds_loop() {
    let context = || context(None, &["server", "a", "mixin"]);
    assert_eq!(
        render(
            "{% for f in features %}{{ f }}{% if not loop.last %},{% endif %}{% endfor %}",
            context()
        )
        .as_deref(),
        Ok("a,mixin,server")
    );
    assert_eq!(
        render(
            "{% for f in features %}{{ loop.index }}/{{ loop.length }}{% endfor %}",
            context()
        )
        .as_deref(),
        Ok("1/32/33/3")
    );
    assert_eq!(
        render(
            "{% for f in features %}{{ loop.revindex }}{% endfor %}",
            context()
        )
        .as_deref(),
        Ok("321")
    );
    // A map yields its keys, as Jinja does; a scalar yields nothing and says so.
    assert_eq!(
        render("{% for k in package %}{{ k }};{% endfor %}", context()).as_deref(),
        Ok("name;version;")
    );
    assert_eq!(
        message("{% for c in package.name %}{{ c }}{% endfor %}", context()),
        "line 1, column 1: a string cannot be iterated"
    );
}

#[test]
fn filters_apply_in_a_chain_and_an_unknown_one_is_refused() {
    let context = || context(Some("1.0"), &["b", "a"]);
    assert_eq!(
        render("{{ package.name | upper }}", context()).as_deref(),
        Ok("HELLOMOD")
    );
    assert_eq!(
        render("{{ package.name | upper | lower | trim }}", context()).as_deref(),
        Ok("hellomod")
    );
    assert_eq!(
        render("{{ features | join(\", \") }}", context()).as_deref(),
        Ok("a, b")
    );
    assert_eq!(
        render("{{ features | length }}", context()).as_deref(),
        Ok("2")
    );
    assert_eq!(
        render(
            "{{ package.name | replace(\"hello\", \"bye\") }}",
            context()
        )
        .as_deref(),
        Ok("byemod")
    );
    assert_eq!(
        render("{{ features | first }} {{ features | last }}", context()).as_deref(),
        Ok("a b")
    );
    assert_eq!(
        message("{{ package.name | nope }}", context()),
        "line 1, column 1: unknown filter `nope`"
    );
    assert_eq!(
        message("{{ package.name | replace(\"a\") }}", context()),
        "line 1, column 1: `replace` takes at least 2 argument(s)"
    );
}

#[test]
fn a_consumers_own_filter_is_reached_the_same_way() {
    // Nothing a built-in can do is closed to a consumer, which is what makes `Environment::empty`
    // a starting point rather than a crippled one.
    let mut env = Environment::empty();
    env.set_strict_variables(true);
    env.add_filter("shout", |value: &Value, _: &[Value]| {
        Ok(Value::from(alloc_shout(value)))
    });
    assert_eq!(
        env.render_str("{{ package.name | shout }}", context(None, &[]))
            .as_deref(),
        Ok("HELLOMOD!")
    );
    // And `Environment::empty` really is empty: even `default` is a registration.
    assert_eq!(
        env.render_str("{{ package.name | default(\"x\") }}", context(None, &[]))
            .expect_err("nothing is registered")
            .kind(),
        ErrorKind::UnknownFilter
    );
}

fn alloc_shout(value: &Value) -> String {
    let mut text = value.as_str().unwrap_or_default().to_uppercase();
    text.push('!');
    text
}

#[test]
fn the_undefined_ladder_gets_stricter_one_rung_at_a_time() {
    let source = "[{{ package.version }}][{{ package.version | default(\"d\") }}]";
    let cases = [
        (UndefinedBehavior::Lenient, Ok("[][d]")),
        (UndefinedBehavior::Chainable, Ok("[][d]")),
        (
            UndefinedBehavior::SemiStrict,
            Err(ErrorKind::UndefinedError),
        ),
        (UndefinedBehavior::Strict, Err(ErrorKind::UndefinedError)),
    ];
    for (behavior, expected) in cases {
        let mut env = Environment::new();
        env.set_undefined_behavior(behavior);
        let rendered = env.render_str(source, context(None, &[]));
        match expected {
            Ok(text) => assert_eq!(rendered.as_deref(), Ok(text), "{behavior:?}"),
            Err(kind) => assert_eq!(
                rendered.expect_err("this rung refuses").kind(),
                kind,
                "{behavior:?}"
            ),
        }
    }

    // The rung `Strict` adds on its own: even `default` may not be handed an unset value.
    let mut env = Environment::new();
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    assert!(
        env.render_str("{{ package.version | default(\"d\") }}", context(None, &[]))
            .is_err()
    );

    // Reading *through* an unset value is an error everywhere but `Chainable`.
    for (behavior, chains) in [
        (UndefinedBehavior::Lenient, false),
        (UndefinedBehavior::Chainable, true),
        (UndefinedBehavior::SemiStrict, false),
    ] {
        let mut env = Environment::new();
        env.set_undefined_behavior(behavior);
        assert_eq!(
            env.render_str(
                "{{ package.version.major | default(\"d\") }}",
                context(None, &[])
            )
            .is_ok(),
            chains,
            "{behavior:?}"
        );
    }
}

#[test]
fn a_malformed_template_says_where_and_why() {
    let context = || context(None, &[]);
    for (source, expected) in [
        ("a {{ b", "line 1, column 3: `{{` is never closed"),
        (
            "{% if features.a %}x",
            "line 1, column 1: `if` is never closed",
        ),
        (
            "{% endif %}",
            "line 1, column 1: `endif` has no block to close",
        ),
        (
            "{% while features.a %}{% endwhile %}",
            "line 1, column 1: unknown tag `while`",
        ),
        (
            "{% if features.a %}x{% endfor %}",
            "line 1, column 21: `endfor` has no block to close",
        ),
        (
            "{% for x features %}{% endfor %}",
            "line 1, column 1: expected `for <name> in <expression>`",
        ),
        (
            "{% if 1 < 2 < 3 %}x{% endif %}",
            "line 1, column 1: comparisons do not chain; write `and` between them",
        ),
    ] {
        let error = strict()
            .render_str(source, context())
            .expect_err("this template is malformed");
        assert_eq!(error.kind(), ErrorKind::SyntaxError, "{source:?}");
        assert_eq!(error.to_string(), expected, "{source:?}");
    }

    let error = render("ok\n{% if %}\n{% endif %}\n", context()).expect_err("no condition");
    assert_eq!((error.line(), error.column()), (Some(2), Some(1)));
}

#[test]
fn deeply_nested_blocks_are_refused_rather_than_recursed() {
    // Malformed input must never take the process down with it.
    let mut source = String::new();
    for _ in 0..80 {
        source.push_str("{% if features.a %}");
    }
    for _ in 0..80 {
        source.push_str("{% endif %}");
    }
    assert_eq!(
        message(&source, context(None, &[])),
        "line 1, column 1217: blocks are nested too deeply"
    );
}

#[test]
fn a_named_template_labels_its_own_errors_and_can_be_included() {
    let mut env = strict();
    env.add_template("body", "  <{{ package.name }}>\n")
        .expect("the source parses");
    env.add_template("page", "start\n{% include \"body\" %}\nend")
        .expect("the source parses");

    assert_eq!(
        env.get_template("page")
            .expect("it was added")
            .render(context(None, &[]))
            .as_deref(),
        Ok("start\n  <hellomod>\nend")
    );
    assert_eq!(env.template_names().collect::<Vec<_>>(), ["body", "page"]);

    // An error names the template it happened in, innermost first.
    env.add_template("broken", "{{ nope }}")
        .expect("the source parses");
    env.add_template("outer", "{% include \"broken\" %}")
        .expect("the source parses");
    let error = env
        .get_template("outer")
        .expect("it was added")
        .render(context(None, &[]))
        .expect_err("the included template fails");
    assert_eq!(error.name(), Some("broken"));

    // A name nothing was added under is refused rather than rendered as nothing.
    assert_eq!(
        env.get_template("missing")
            .expect_err("nothing there")
            .kind(),
        ErrorKind::TemplateNotFound
    );
    env.add_template("cycle", "{% include \"cycle\" %}")
        .expect("the source parses");
    assert_eq!(
        env.get_template("cycle")
            .expect("it was added")
            .render(context(None, &[]))
            .expect_err("a template that includes itself never ends")
            .kind(),
        ErrorKind::InvalidOperation
    );
}

#[test]
fn the_context_macro_builds_the_map_a_render_reads() {
    let name = "hellomod";
    let rendered = Environment::new()
        .render_str(
            "{{ name }}/{{ version }}/{{ count }}/{{ on }}",
            context! { name, version => "1.0", count => 3, on => true },
        )
        .expect("it renders");
    assert_eq!(rendered, "hellomod/1.0/3/true");
    assert_eq!(
        Environment::new()
            .render_str("{{ nothing | default(\"-\") }}", context! {})
            .as_deref(),
        Ok("-")
    );
}

#[test]
fn a_global_is_read_by_every_template_and_listed_beside_the_context() {
    let mut env = strict();
    env.add_global("tool", Value::from("jals"));
    assert_eq!(
        env.render_str("{{ tool }}", context! {}).as_deref(),
        Ok("jals")
    );
    // A loop binding shadows the context, and the context shadows a global.
    assert_eq!(
        env.render_str(
            "{% for tool in items %}{{ tool }}{% endfor %}{{ tool }}",
            context! { items => Value::from(vec![Value::from("a")]), tool => "ctx" }
        )
        .as_deref(),
        Ok("actx")
    );
    assert_eq!(
        env.render_str("{{ nope }}", context! { here => 1 })
            .expect_err("no such name")
            .to_string(),
        "line 1, column 1: unknown name `nope`; a template can read `here` and `tool`"
    );
}

#[test]
fn not_denies_the_comparison_rather_than_its_left_operand() {
    // Jinja's precedence is `or` < `and` < `not` < comparison < filter. Binding `not` tighter than
    // the comparison reads `(not a) == b`, which for a boolean and a string is `false` whatever
    // either holds — the wrong arm, silently, on a template shape a build tool advertises.
    let source = "{% if not package.version == \"1.0\" %}NOT{% else %}IS{% endif %}";
    assert_eq!(
        render(source, context(Some("1.0"), &[])).as_deref(),
        Ok("IS")
    );
    assert_eq!(
        render(source, context(Some("2.0"), &[])).as_deref(),
        Ok("NOT")
    );

    // Filters still bind tighter than `not`, so this is `not (version | default(""))`.
    assert_eq!(
        render(
            "{{ not package.version | default(\"\") }}",
            context(None, &[])
        )
        .as_deref(),
        Ok("true")
    );
    // …and `and` still binds looser, so this is `(not a) and b`.
    assert_eq!(
        render("{{ not features.a and features.b }}", context(None, &["b"])).as_deref(),
        Ok("true")
    );
    assert_eq!(
        render("{{ not not features.a }}", context(None, &["a"])).as_deref(),
        Ok("true")
    );
}

#[test]
fn a_loop_variable_may_not_be_a_name_an_expression_already_spells() {
    // Each of these binds every item and is then read as something else — the literal, or the
    // namespace pushed after it — so the template renders successfully and renders the wrong
    // bytes. Refused at parse time instead.
    for reserved in ["none", "true", "false", "loop", "and", "or", "not"] {
        let source = for_binding(reserved);
        let error = render(&source, context(None, &["a", "b"])).expect_err("the binding is taken");
        assert_eq!(error.kind(), ErrorKind::SyntaxError, "{reserved}");
        assert_eq!(
            error.detail(),
            Some(
                format!(
                    "`{reserved}` already means something in an expression, so it cannot be a \
                     loop variable"
                )
                .as_str()
            ),
            "{reserved}"
        );
    }
    // A name that is not taken still binds.
    assert_eq!(
        render(
            "{% for f in features %}{{ f }},{% endfor %}",
            context(None, &["a", "b"])
        )
        .as_deref(),
        Ok("a,b,")
    );
}

fn for_binding(binding: &str) -> String {
    format!("{{% for {binding} in features %}}{{{{ {binding} }}}},{{% endfor %}}")
}

#[test]
fn a_field_name_may_begin_with_a_digit_but_a_literal_still_wins_everywhere_else() {
    // `8` and `1_20` are the shape of a real feature name (a Minecraft-style project declares
    // versions as features), and neither is a number the template meant to write.
    let context = || context(None, &["8", "1_20"]);
    assert_eq!(render("{{ features.8 }}", context()).as_deref(), Ok("true"));
    assert_eq!(
        render("{{ features.1_20 }}", context()).as_deref(),
        Ok("true")
    );
    assert_eq!(
        render("{% if features.2 %}Y{% else %}N{% endif %}", context()).as_deref(),
        Ok("N")
    );

    // Outside field position a leading digit is still a literal.
    assert_eq!(render("{{ 1.5 }}", context()).as_deref(), Ok("1.5"));
    assert_eq!(
        render("{{ items[0] }}", items_context()).as_deref(),
        Ok("a")
    );
    // A number followed by a field access hands the `.` back rather than swallowing it, so the
    // failure is the access it is rather than a stray trailing token.
    assert_eq!(
        message("{{ 1.name }}", context()),
        "line 1, column 1: this value has no fields, so `name` cannot be read"
    );
}

fn items_context() -> Value {
    context! { items => Value::from(vec![Value::from("a"), Value::from("b")]) }
}

#[test]
fn a_number_literal_that_does_not_fit_is_refused_whether_or_not_it_has_a_point() {
    // `f64::from_str` answers `Ok(inf)` rather than failing, so without the finiteness check one
    // of these two spellings would write `inf` into the rendered document and the other would not.
    let huge = "9".repeat(400);
    for source in [format!("{{{{ {huge} }}}}"), format!("{{{{ {huge}.0 }}}}")] {
        assert_eq!(
            message(&source, context(None, &[])),
            "line 1, column 1: this number does not fit"
        );
    }
}

#[test]
fn default_with_no_argument_leaves_the_value_unset_rather_than_emptying_it() {
    // `| default` with nothing to fall back on is not a fallback, so a stricter rung still gets to
    // refuse it — which is the whole reason a caller picks one.
    for source in [
        "{{ package.version | default }}",
        "{{ package.version | default() }}",
    ] {
        let error = render(source, context(None, &[])).expect_err("nothing was given to use");
        assert_eq!(error.kind(), ErrorKind::UndefinedError, "{source:?}");
    }
    // Lenient writes the empty string for an unset value anyway, so the Jinja spelling still reads
    // the way Jinja reads it.
    let mut lenient = Environment::new();
    lenient.set_undefined_behavior(UndefinedBehavior::Lenient);
    assert_eq!(
        lenient
            .render_str("[{{ nothing | default() }}]", context! {})
            .as_deref(),
        Ok("[]")
    );
}

#[test]
fn a_filter_names_itself_and_says_when_its_subject_is_merely_unset() {
    // `count` is `length` under another name, and an error from it has to name the one that was
    // written — the fix for `count(1)` is not to go and read about `length`.
    let error = Environment::new()
        .render_str("{{ \"x\" | count(1) }}", context! {})
        .expect_err("count takes no arguments");
    assert_eq!(error.kind(), ErrorKind::TooManyArguments);
    assert_eq!(error.detail(), Some("`count` takes at most 0 argument(s)"));

    // Whether a filter exists is settled before what its subject holds, or the strictest rung
    // reports a misspelled name as an unset value and asserts a filter nobody registered.
    let mut strictest = Environment::new();
    strictest.set_undefined_behavior(UndefinedBehavior::Strict);
    let error = strictest
        .render_str("{{ nothing | dfault(\"z\") }}", context! {})
        .expect_err("no such filter");
    assert_eq!(error.kind(), ErrorKind::UnknownFilter);
    assert_eq!(error.detail(), Some("unknown filter `dfault`"));

    // *Unset* and *no text form* are the crate's two different mistakes, and only one of them has
    // a `| default(…)` fix. A filter must not hand the other one's message to it.
    let error = render("{{ package.version | upper }}", context(None, &[]))
        .expect_err("the value is not set");
    assert_eq!(error.kind(), ErrorKind::UndefinedError);
    let error = render(
        "{{ \"z\" | replace(package.version, \"b\") }}",
        context(None, &[]),
    )
    .expect_err("the argument is not set");
    assert_eq!(error.kind(), ErrorKind::UndefinedError);
}

#[test]
fn an_integer_and_a_float_are_one_shape_and_every_equal_pair_is_ordered() {
    let context = items_context;
    // `ValueKind::Number` covers both, so refusing to compare them would report that a number
    // cannot be compared with a number.
    assert_eq!(render("{{ 1 == 1.0 }}", context()).as_deref(), Ok("true"));
    assert_eq!(render("{{ 1 < 1.5 }}", context()).as_deref(), Ok("true"));
    assert_eq!(render("{{ 2 >= 1.5 }}", context()).as_deref(), Ok("true"));
    assert_eq!(
        render(
            "{% if items | length > 1.5 %}Y{% else %}N{% endif %}",
            context()
        )
        .as_deref(),
        Ok("Y")
    );

    // `a == b` has to imply `partial_cmp(a, b) == Some(Equal)`, or a consumer sorting a
    // `Vec<Value>` gets nonsense for the shapes that carry no order of their own.
    for (left, right) in [
        (Value::UNDEFINED, Value::UNDEFINED),
        (Value::NONE, Value::NONE),
        (
            Value::from(vec![Value::from("a")]),
            Value::from(vec![Value::from("a")]),
        ),
        (Value::from(1i64), Value::from(1.0f64)),
    ] {
        assert!(left == right, "{left:?} == {right:?}");
        assert_eq!(
            left.partial_cmp(&right),
            Some(std::cmp::Ordering::Equal),
            "{left:?} vs {right:?}"
        );
    }

    // The integer side is compared exactly, so a magnitude no `f64` can hold still orders.
    assert_eq!(
        Value::from(i64::MAX).partial_cmp(&Value::from(1e300f64)),
        Some(std::cmp::Ordering::Less)
    );
    assert_eq!(
        Value::from(-1i64).partial_cmp(&Value::from(-1.5f64)),
        Some(std::cmp::Ordering::Greater)
    );
}
