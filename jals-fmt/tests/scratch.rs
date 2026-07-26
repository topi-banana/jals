//! Scratch probe used while converging the rules; not a regression test.

use jals_config::fmt::Config;

fn fmt(src: &str, cfg: &Config) -> String {
    jals_exec::block_on_inline(jals_fmt::FormatOutput::format_source(src, cfg)).formatted
}

#[test]
#[ignore = "developer probe"]
fn probe() {
    let src = "class T {\n  /* a block comment whose prose is long enough that reflowing it against a width would move it */\n  // a line comment whose prose is also long enough that reflowing it against a width would move it\n  /** A javadoc comment with prose long enough to need refilling against the configured width. */\n  void m() {}\n}\n";
    let mut on = Config::default();
    on.comments.format_line = true;
    on.comments.format_block = true;
    on.comments.format_javadoc = true;
    on.comments.format_header = true;
    println!("=== OFF ===\n{}", fmt(src, &Config::default()));
    println!("=== ON ===\n{}", fmt(src, &on));

    // Exactly what the coverage test does for `comments.format-block`.
    let ks = std::fs::read_to_string("tests/kitchen.java").unwrap_or_default();
    let mut off = on.clone();
    off.comments.format_block = false;
    println!("=== KS DIFFERS: {} ===", fmt(&ks, &on) != fmt(&ks, &off));
    println!("{}", fmt(&ks, &on));
}
