//! `jals-lint/jalslint.toml` is the documented sample config, so it has to be a config this
//! release actually accepts.
//!
//! It is the file a reader copies. A sample that names a rule the schema dropped, or spells an
//! option value the enum no longer has, would load *silently* — unknown keys are kept rather than
//! rejected, on purpose — and the reader would find out only from behaviour that never changed. So
//! the test asserts not merely that it parses but that it configures nothing unknown, and that
//! every value in it is the built-in one, which is what makes it readable as documentation of the
//! defaults.

use jals_config::lint::Config;

#[test]
fn the_sample_config_is_the_default_config() {
    let sample: Config = toml::from_str(include_str!("../jalslint.toml")).expect("valid TOML");
    assert_eq!(
        sample.unknown_keys(),
        Vec::<String>::new(),
        "the sample names a key this schema does not define"
    );
    assert_eq!(
        sample,
        Config::default(),
        "the sample states the built-in values, so it must deserialize to them"
    );
}
