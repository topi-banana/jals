//! `jals toolchain`: the project-local JDK installer and version manager.
//!
//! Toolchains live under the project's `target/jdk` ([`jals_config::MANAGED_TOOLCHAIN_ROOT`]) rather
//! than in a user-level directory, so installing one never changes the host's Java and everything
//! an install writes is covered by the same ignore rule as the rest of `target/`. A manifest then
//! selects one with the `[toolchain]` table it already has:
//!
//! ```toml
//! [toolchain]
//! compiler = { distribution = { name = "temurin", version = 21 } }
//! runtime  = { distribution = { name = "temurin", version = 21 } }
//! ```
//!
//! `jals build`/`run`/`test` install a selected distribution that is not present yet
//! (rust-toolchain.toml style), unless `--offline` refuses the network — the manifest alone is
//! enough to make a fresh checkout build. The same store is managed by hand through `install`,
//! `list`, `uninstall`, `which`, `link`, and `default`; a toolchain installed by any of them is
//! what a distribution selector matches, and `default` additionally claims the `system` selection
//! for this project.
//!
//! The install name is the requested spec (`temurin-21`), not the exact patch the provider
//! resolved, because that name is what a manifest selector matches and what a repeat `install`
//! refreshes in place. The exact release is recorded beside it in the install's metadata, which is
//! what `list` shows.

mod archive;
mod provider;

use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, anyhow, bail};
use clap::{Args, Subcommand};
use jals_build::JdkInstall;
use jals_classpath::{ExternalLocator, Fetch, ReqwestFetcher};
use jals_config::{MANAGED_TOOLCHAIN_ROOT, Manifest, ToolSpec};
use jals_exec::tokio_rt::on_blocking_pool;
use jals_progress::{Activity, Outcome};

use archive::{Archive, JdkHome};
use provider::{Provider, Release};

use crate::session::Session;
use crate::shell::Verb;

/// The `jals toolchain` command group.
#[derive(Args)]
pub(crate) struct ToolchainArgs {
    /// Use this manifest instead of discovering `jals.toml` upward from the cwd.
    #[arg(long, value_name = "PATH", global = true)]
    manifest_path: Option<PathBuf>,

    /// Never fetch provider metadata or an archive over the network. A `file://` install URL is
    /// still read.
    #[arg(long, global = true)]
    offline: bool,

    /// Extra attempts a transient network failure (a timeout, a refused connection, a 5xx) is
    /// given before the fetch fails. `0` disables retrying.
    #[arg(long, value_name = "N", default_value_t = jals_classpath::RetrySchedule::DEFAULT_RETRIES, global = true)]
    network_retry: u32,

    #[command(subcommand)]
    command: ToolchainCommand,
}

#[derive(Subcommand)]
enum ToolchainCommand {
    /// Download and install a JDK under `target/jdk`.
    Install(InstallArgs),
    /// List installed toolchains, or the versions a distribution offers.
    List(ListArgs),
    /// Remove installed toolchains matching a spec.
    Uninstall(UninstallArgs),
    /// Print the JDK home a spec resolves to among the installed toolchains.
    Which(WhichArgs),
    /// Register an existing JDK directory as a toolchain.
    Link(LinkArgs),
    /// Show or set the project's default toolchain for `system` selections.
    Default(DefaultArgs),
}

#[derive(Args)]
struct InstallArgs {
    /// `[distribution@]version` — e.g. `temurin@21`, `zulu@17`, `21` (defaults to temurin), or
    /// `temurin@lts`.
    spec: String,

    /// Fetch this archive instead of resolving one through a provider. `file://` works offline;
    /// the archive kind is sniffed from its bytes.
    #[arg(long, value_name = "URL")]
    url: Option<String>,

    /// Reinstall even when the toolchain is already present.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct ListArgs {
    /// With `--available`, the distribution to query (`temurin`, `zulu`, `openjdk`, …). Defaults
    /// to `temurin`.
    distribution: Option<String>,

    /// List the major versions a distribution offers instead of what is installed.
    #[arg(long)]
    available: bool,
}

#[derive(Args)]
struct UninstallArgs {
    /// `[distribution@]version` naming what to remove, as `install` accepts.
    spec: String,
}

#[derive(Args)]
struct WhichArgs {
    /// `[distribution@]version` naming the install, as `install` accepts.
    spec: String,
}

#[derive(Args)]
struct LinkArgs {
    /// The toolchain name to register (`temurin-21`, `my-jdk`, …).
    name: String,

    /// An existing JDK home directory.
    path: PathBuf,

    /// Replace an existing toolchain with this name.
    #[arg(long)]
    force: bool,
}

#[derive(Args)]
struct DefaultArgs {
    /// The toolchain to make this project's default, as `install` accepts. When it is not
    /// installed yet it is installed first.
    spec: Option<String>,

    /// Remove the project's default.
    #[arg(long, conflicts_with = "spec")]
    unset: bool,
}

impl ToolchainArgs {
    /// Dispatch the selected subcommand.
    pub(crate) async fn run(&self, session: &Session) -> Result<ExitCode> {
        match &self.command {
            ToolchainCommand::Install(args) => args.run(self, session).await,
            ToolchainCommand::List(args) => args.run(self, session).await,
            ToolchainCommand::Uninstall(args) => args.run(self, session).await,
            ToolchainCommand::Which(args) => args.run(self, session).await,
            ToolchainCommand::Link(args) => args.run(self, session).await,
            ToolchainCommand::Default(args) => args.run(self, session).await,
        }
    }

    /// Discover the project, name it for reports, and open its toolchain store.
    async fn project(&self, session: &Session) -> Result<(Manifest, PathBuf, Store)> {
        let (manifest, root) = crate::App::resolve_manifest(self.manifest_path.as_deref()).await?;
        session.note_project(&root, manifest.package.name.as_deref());
        let store = Store::new(&root);
        Ok((manifest, root, store))
    }

    /// The fetch capability every network step of this command uses.
    fn fetcher(&self, root: &Path) -> ReqwestFetcher {
        ReqwestFetcher::for_project(
            root.to_path_buf(),
            jals_classpath::NetworkPolicy::when_offline(self.offline),
            jals_classpath::RetrySchedule::new(self.network_retry),
        )
    }
}

impl InstallArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        let spec = Spec::parse(&self.spec)
            .map_err(|error| anyhow!("invalid toolchain `{}`: {error}", self.spec))?;
        let (_, root, store) = args.project(session).await?;
        let fetcher = args.fetcher(&root);
        store
            .install(&spec, self.url.as_deref(), &fetcher, session, self.force)
            .await?;
        session.finished(&format!("installing {}", spec.install_name()));
        Ok(ExitCode::SUCCESS)
    }
}

impl ListArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        if self.available {
            return self.available(args, session).await;
        }
        let (_, _root, store) = args.project(session).await?;
        session.stdout_is_free("`jals toolchain list`")?;
        let entries = store.entries();
        if entries.is_empty() {
            session.shell().machine("no toolchains installed");
            return Ok(ExitCode::SUCCESS);
        }
        let default = store.default_name();
        for entry in entries {
            let mut line = entry.name.clone();
            if let Some(release) = &entry.release {
                line.push_str(" (");
                line.push_str(release);
                line.push(')');
            }
            if entry.linked {
                line.push_str(" [linked]");
            }
            if default.as_deref() == Some(entry.name.as_str()) {
                line.push_str(" [default]");
            }
            session.shell().machine(line);
        }
        Ok(ExitCode::SUCCESS)
    }

    /// The remote listing: which major versions a distribution publishes as GA builds.
    ///
    /// Deliberately does not need a manifest — the question is about the provider, not the
    /// project — so this is the one toolchain subcommand that runs outside one.
    async fn available(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        let distribution = Spec::canonical_distribution(self.distribution.as_deref().unwrap_or(""))
            .map_err(|error| anyhow!("{error}"))?;
        let root = std::env::current_dir().context("getting current dir")?;
        let fetcher = args.fetcher(&root);
        let majors = Provider::for_distribution(&distribution)
            .majors(&distribution, &fetcher)
            .await?;
        session.stdout_is_free("`jals toolchain list --available`")?;
        if majors.is_empty() {
            session
                .shell()
                .machine(format!("{distribution}: no GA releases"));
            return Ok(ExitCode::SUCCESS);
        }
        for major in majors {
            session.shell().machine(major);
        }
        Ok(ExitCode::SUCCESS)
    }
}

impl UninstallArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        let spec = Spec::parse(&self.spec)
            .map_err(|error| anyhow!("invalid toolchain `{}`: {error}", self.spec))?;
        let (_, _root, store) = args.project(session).await?;
        let entries = store.find(&spec);
        if entries.is_empty() {
            bail!(
                "no installed toolchain matches `{}`; run `jals toolchain list`",
                self.spec
            );
        }
        let default = store.default_name();
        for entry in &entries {
            Store::remove(entry).with_context(|| format!("removing {}", entry.home.display()))?;
            if default.as_deref() == Some(entry.name.as_str()) {
                store
                    .clear_default()
                    .with_context(|| format!("clearing the default toolchain `{}`", entry.name))?;
            }
            session.shell().status(Verb::Removing, &entry.name);
        }
        session.finished("uninstalling toolchain");
        Ok(ExitCode::SUCCESS)
    }
}

impl WhichArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        let spec = Spec::parse(&self.spec)
            .map_err(|error| anyhow!("invalid toolchain `{}`: {error}", self.spec))?;
        let (_, _root, store) = args.project(session).await?;
        let entries = store.find(&spec);
        let Some(entry) = entries.first() else {
            bail!(
                "`{}` is not installed; run `jals toolchain install {}`",
                self.spec,
                spec.request()
            );
        };
        session.stdout_is_free("`jals toolchain which`")?;
        session.shell().machine(entry.home.display());
        Ok(ExitCode::SUCCESS)
    }
}

impl LinkArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        Store::validate_name(&self.name)?;
        let (_, _root, store) = args.project(session).await?;
        let home = std::fs::canonicalize(&self.path)
            .with_context(|| format!("resolving {}", self.path.display()))?;
        if !JdkHome::is_jdk(&home) {
            bail!(
                "{} is not a JDK home: no `bin/{}`",
                home.display(),
                JdkHome::javac()
            );
        }
        let destination = store.dir().join(&self.name);
        if std::fs::symlink_metadata(&destination).is_ok() {
            if !self.force {
                bail!(
                    "`{}` is already registered; pass `--force` to replace it",
                    self.name
                );
            }
            Store::remove(&store.entry(self.name.clone()))
                .with_context(|| format!("replacing `{}`", self.name))?;
        }
        std::fs::create_dir_all(store.dir())
            .with_context(|| format!("creating {}", store.dir().display()))?;
        Self::symlink(&home, &destination)
            .with_context(|| format!("linking {}", destination.display()))?;
        session.shell().status(
            Verb::Created,
            format_args!("{} -> {}", self.name, home.display()),
        );
        Ok(ExitCode::SUCCESS)
    }

    /// A directory link to `target`, named `link`.
    #[cfg(unix)]
    fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(target, link)
    }

    /// The Windows spelling. Creating a directory symlink may need a privilege; the failure
    /// names it, and `install` is the fallback that needs none.
    #[cfg(windows)]
    fn symlink(target: &Path, link: &Path) -> io::Result<()> {
        std::os::windows::fs::symlink_dir(target, link)
    }
}

impl DefaultArgs {
    async fn run(&self, args: &ToolchainArgs, session: &Session) -> Result<ExitCode> {
        let (_, _root, store) = args.project(session).await?;
        if self.unset {
            if store.default_name().is_some() {
                store
                    .clear_default()
                    .context("clearing the default toolchain")?;
                session.shell().status(Verb::Removing, "default toolchain");
            }
            return Ok(ExitCode::SUCCESS);
        }
        let Some(raw) = &self.spec else {
            session.stdout_is_free("`jals toolchain default`")?;
            let line = store.default_name().map_or_else(
                || "no default toolchain".to_owned(),
                |name| format!("{name} -> {}", store.dir().join(&name).display()),
            );
            session.shell().machine(line);
            return Ok(ExitCode::SUCCESS);
        };
        let spec =
            Spec::parse(raw).map_err(|error| anyhow!("invalid toolchain `{raw}`: {error}"))?;
        let entry = if let Some(entry) = store.find(&spec).into_iter().next() {
            entry
        } else {
            // rustup's `default` installs what it names; so does this one, so pointing a fresh
            // checkout at a toolchain is one command rather than two.
            let fetcher = args.fetcher(&store.root);
            store.install(&spec, None, &fetcher, session, false).await?;
            store.find(&spec).into_iter().next().ok_or_else(|| {
                anyhow!(
                    "the install of `{}` did not publish it",
                    spec.install_name()
                )
            })?
        };
        store
            .set_default(&entry.name)
            .with_context(|| format!("setting the default to `{}`", entry.name))?;
        session
            .shell()
            .status(Verb::Created, format_args!("default -> {}", entry.name));
        Ok(ExitCode::SUCCESS)
    }
}

/// The JDK selections one command's work reaches.
///
/// Stated by the caller rather than derived here, because only the caller knows which steps it
/// will run: `jals test --no-run` compiles and stops, `jals run` on a wasm backend runs no `java`,
/// and an in-process backend reaches no JDK at all.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Needs {
    /// The command will run the `javac` backend.
    pub(crate) compiler: bool,
    /// The command will spawn a `java` process.
    pub(crate) runtime: bool,
}

/// The `[toolchain]` side of a build command: make every distribution selection it is about to
/// use resolvable, installing the ones that are not.
pub(crate) struct Toolchain;

impl Toolchain {
    /// Install whatever `needs` names and the project does not already have.
    ///
    /// A selection that matches any discovered install — the project store or one of the host's
    /// SDKMAN/IntelliJ/`/usr/lib/jvm` JDKs — is left alone: the point is to make an explicit
    /// `distribution` selector resolvable, not to prefer this crate's copy of a JDK the machine
    /// already has. A selector with no `version` cannot be downloaded (there is no build to pick),
    /// so it is refused by name rather than silently falling back to the host's tools.
    pub(crate) async fn ensure(
        manifest: &Manifest,
        root: &Path,
        fetcher: &ReqwestFetcher,
        session: &Session,
        needs: Needs,
    ) -> Result<()> {
        let mut wanted: Vec<Spec> = Vec::new();
        if needs.compiler
            && let Some(ToolSpec::Distribution { name, version }) =
                manifest.toolchain.compiler.spec()
        {
            wanted.push(
                Spec::from_selector(name, version)
                    .map_err(|error| anyhow!("`[toolchain] compiler`: {error}"))?,
            );
        }
        if needs.runtime
            && let Some(ToolSpec::Distribution { name, version }) =
                manifest.toolchain.runtime.spec()
        {
            wanted.push(
                Spec::from_selector(name, version)
                    .map_err(|error| anyhow!("`[toolchain] runtime`: {error}"))?,
            );
        }
        wanted.dedup();
        if wanted.is_empty() {
            return Ok(());
        }
        let store = Store::new(root);
        let installed = JdkInstall::discover(Some(root));
        for spec in wanted {
            if installed
                .iter()
                .any(|install| install.satisfies(Some(&spec.distribution), spec.major()))
            {
                continue;
            }
            store
                .install(&spec, None, fetcher, session, false)
                .await
                .with_context(|| {
                    format!(
                        "`[toolchain]` selects `{}`, which is not installed and could not be \
                         downloaded; install it with `jals toolchain install {}`",
                        spec.install_name(),
                        spec.request()
                    )
                })?;
        }
        Ok(())
    }
}

/// A parsed `[distribution@]version` toolchain request.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    /// The distribution/vendor, canonical (`temurin`, `zulu`, …).
    distribution: String,
    /// Which version was asked for.
    version: Version,
}

/// The version half of a [`Spec`].
#[derive(Debug, Clone, PartialEq, Eq)]
enum Version {
    /// The newest feature release (`latest`).
    Latest,
    /// The newest long-term-support release (`lts`).
    Lts,
    /// A major version (`21`).
    Major(u32),
    /// An exact release (`21.0.12.1+1`).
    Exact(String),
}

impl Spec {
    /// What a spec without a distribution names.
    const DEFAULT_DISTRIBUTION: &'static str = "temurin";

    /// Parse `[distribution@]version`.
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let (distribution, version) = match raw.split_once('@') {
            Some((distribution, version)) => (distribution, version),
            // An install name (`temurin-21`) is what `list` prints and what a user copies, so it
            // parses too. The split is at the first `-` that starts a version-like tail and only
            // when the head looks like a distribution, which keeps an exact version that happens
            // to carry a build suffix (`8u422-b05`) whole.
            None => match raw.split_once('-') {
                Some((distribution, version))
                    if !distribution.is_empty()
                        && !distribution.chars().any(|c| c.is_ascii_digit())
                        && version.starts_with(|c: char| c.is_ascii_digit()) =>
                {
                    (distribution, version)
                }
                _ => ("", raw),
            },
        };
        if version.is_empty() {
            return Err(
                "expected `[distribution@]version`, e.g. `temurin@21`, `zulu@lts`, or `21`"
                    .to_owned(),
            );
        }
        Ok(Self {
            distribution: Self::canonical_distribution(distribution)?,
            version: Version::parse(version)?,
        })
    }

    /// The spec a manifest's `Distribution` selector names.
    ///
    /// A selector's `version` is a major (`21`), which is enough: the install is named for the
    /// request and the provider picks the newest GA build of it.
    fn from_selector(name: Option<&str>, version: Option<u32>) -> Result<Self, String> {
        let Some(version) = version else {
            return Err(
                "the selector names no `version`, and an unversioned one matches whatever is \
                 installed — there is no build to download. Add `version = 21`, or run `jals \
                 toolchain install <distribution>@<version>` first."
                    .to_owned(),
            );
        };
        Ok(Self {
            distribution: Self::canonical_distribution(name.unwrap_or(""))?,
            version: Version::Major(version),
        })
    }

    /// A distribution name in the one spelling the providers, discovery, and selection share.
    ///
    /// The vendor's own aliases collapse onto the canonical name — the same rule
    /// [`JdkInstall::canonical_distribution`] gives the resolver, so a spec parsed here resolves
    /// the install it names (`adoptium`/`adoptopenjdk`/`eclipse` are all Temurin). An empty name
    /// is the default, and the result is lowercased so `Temurin@21` and `temurin@21` are one
    /// install.
    fn canonical_distribution(raw: &str) -> Result<String, String> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Ok(Self::DEFAULT_DISTRIBUTION.to_owned());
        }
        if raw.contains(['/', '\\', '@', ' ']) {
            return Err(format!("`{raw}` is not a distribution name"));
        }
        Ok(JdkInstall::canonical_distribution(raw))
    }

    /// The directory name an install of this spec gets.
    ///
    /// The requested version, not the resolved one: a manifest selector matches this name, and a
    /// repeat `install temurin@21` has to refresh the same directory rather than pile up patch
    /// releases no selector can choose between.
    fn install_name(&self) -> String {
        format!("{}-{}", self.distribution, self.version.label())
    }

    /// The spec as a command-line argument (`temurin@21`).
    ///
    /// Distinct from [`install_name`](Self::install_name): that is a directory (`temurin-21`), and
    /// handing it back as something to *run* parses as the default distribution with a version of
    /// `temurin-21`. Every hint that tells a user what to type uses this one.
    fn request(&self) -> String {
        format!("{}@{}", self.distribution, self.version.label())
    }

    /// The major version this spec resolves to, when one is stated.
    fn major(&self) -> Option<u32> {
        match &self.version {
            Version::Major(major) => Some(*major),
            Version::Exact(version) => Version::major_of(version),
            Version::Latest | Version::Lts => None,
        }
    }
}

impl Version {
    /// Parse the version half of a spec.
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "latest" => Ok(Self::Latest),
            "lts" => Ok(Self::Lts),
            _ if raw.chars().all(|c| c.is_ascii_digit()) => raw
                .parse()
                .map(Self::Major)
                .map_err(|_| format!("`{raw}` is not a Java major version")),
            // An exact release must carry a digit: every Java version does, and without the check
            // `temurin@two` would reach the provider as an exact release rather than being refused
            // at the command line.
            _ if raw.chars().any(|c| c.is_ascii_digit())
                && raw
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '+' | '-' | '_')) =>
            {
                Ok(Self::Exact(raw.to_owned()))
            }
            _ => Err(format!(
                "`{raw}` is not a version: expected a major (`21`), an exact release \
                 (`21.0.12.1+1`), `latest`, or `lts`"
            )),
        }
    }

    /// The spelling that names this request in an install directory.
    fn label(&self) -> String {
        match self {
            Self::Latest => "latest".to_owned(),
            Self::Lts => "lts".to_owned(),
            Self::Major(major) => major.to_string(),
            Self::Exact(version) => version.clone(),
        }
    }

    /// The leading major version of a release string (`21.0.12.1+1` → 21, `8u422-b05` → 8).
    fn major_of(version: &str) -> Option<u32> {
        let start = version.find(|c: char| c.is_ascii_digit())?;
        let run: String = version[start..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        run.parse().ok()
    }
}

/// The project-local JDK store (`<root>/target/jdk`).
struct Store {
    /// The project root the store hangs off.
    root: PathBuf,
}

/// One installed toolchain.
struct Entry {
    /// The install directory's name, which is the toolchain's name (`temurin-21`).
    name: String,
    /// The JDK home the name resolves to.
    home: PathBuf,
    /// The exact release the install resolved, when its metadata records one.
    release: Option<String>,
    /// Whether this entry is a link registered by `jals toolchain link`.
    linked: bool,
}

impl Store {
    /// The metadata file written beside a downloaded JDK's home.
    const METADATA_FILE: &'static str = ".jals-toolchain.json";
    /// The marker file naming the default toolchain.
    const DEFAULT_FILE: &'static str = "default";
    /// The ceiling on an archive held in memory before extraction.
    ///
    /// A JDK build is tens to a few hundred megabytes; this leaves generous headroom and still
    /// catches a URL that answered with something off the scale.
    const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;

    /// The store under `project_root`.
    fn new(project_root: &Path) -> Self {
        Self {
            root: project_root.to_path_buf(),
        }
    }

    /// The store directory itself.
    fn dir(&self) -> PathBuf {
        self.root.join(MANAGED_TOOLCHAIN_ROOT)
    }

    /// Refuse a name that is not a direct child of the store.
    fn validate_name(name: &str) -> Result<()> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || name == Self::DEFAULT_FILE
            || name.contains(['/', '\\'])
        {
            bail!("`{name}` is not a usable toolchain name");
        }
        Ok(())
    }

    /// Every installed toolchain, by name.
    ///
    /// Only this project's store: `which` and `uninstall` are about what was installed *here*,
    /// while distribution *matching* is the wider question [`Toolchain::ensure`] and the build
    /// resolver ask.
    fn entries(&self) -> Vec<Entry> {
        let Ok(read) = std::fs::read_dir(self.dir()) else {
            return Vec::new();
        };
        let mut names: Vec<String> = read
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| !name.starts_with(".tmp-") && name != Self::DEFAULT_FILE)
            .collect();
        names.sort();
        names.into_iter().map(|name| self.entry(name)).collect()
    }

    /// Describe one install directory that is already known to exist.
    fn entry(&self, name: String) -> Entry {
        let home = self.dir().join(&name);
        let linked = std::fs::symlink_metadata(&home)
            .is_ok_and(|metadata| metadata.file_type().is_symlink());
        Entry {
            release: Self::release_of(&home),
            name,
            home,
            linked,
        }
    }

    /// The exact release recorded in an install's metadata, when it has one.
    fn release_of(home: &Path) -> Option<String> {
        let bytes = std::fs::read(home.join(Self::METADATA_FILE)).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        value["release"].as_str().map(str::to_owned)
    }

    /// The installed entries `spec` names, exact name first.
    ///
    /// A spec matches its own install directory exactly, or any installed toolchain the same
    /// distribution/version rule a manifest selector uses would match — so `which temurin@21`
    /// finds an install named `temurin-21.0.12.1+1` too.
    fn find(&self, spec: &Spec) -> Vec<Entry> {
        let exact = spec.install_name();
        let mut matches: Vec<Entry> = self
            .entries()
            .into_iter()
            .filter(|entry| {
                entry.name == exact
                    || JdkInstall::from_install_name(entry.home.clone(), &entry.name)
                        .satisfies(Some(&spec.distribution), spec.major())
            })
            .collect();
        matches.sort_by(|left, right| left.name.cmp(&right.name));
        matches.sort_by_key(|entry| entry.name != exact);
        matches
    }

    /// The install the `default` marker names, when one is set and still installed.
    fn default_name(&self) -> Option<String> {
        let name = std::fs::read_to_string(self.dir().join(Self::DEFAULT_FILE)).ok()?;
        let name = name.trim();
        (!name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..")
            .then(|| name.to_owned())
    }

    /// Point `system` selections at the install named `name`.
    fn set_default(&self, name: &str) -> io::Result<()> {
        std::fs::create_dir_all(self.dir())?;
        std::fs::write(self.dir().join(Self::DEFAULT_FILE), format!("{name}\n"))
    }

    /// Drop the project's default, if it has one.
    fn clear_default(&self) -> io::Result<()> {
        match std::fs::remove_file(self.dir().join(Self::DEFAULT_FILE)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Remove one install.
    fn remove(entry: &Entry) -> io::Result<()> {
        Self::remove_path(&entry.home)
    }

    /// Remove an install path.
    ///
    /// A link is removed as the link it is — following it would delete the JDK the user
    /// registered, which this command never installed and does not own. The check is
    /// `symlink_metadata`, so a *broken* link is still a link rather than an absent path a rename
    /// could not replace.
    fn remove_path(path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() {
            return Self::remove_link(path);
        }
        std::fs::remove_dir_all(path)
    }

    /// Remove a directory link without following it.
    #[cfg(unix)]
    fn remove_link(path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    /// The Windows spelling: a directory symlink is removed as a directory.
    #[cfg(windows)]
    fn remove_link(path: &Path) -> io::Result<()> {
        std::fs::remove_dir(path)
    }

    /// Download, extract, and publish one toolchain.
    ///
    /// `url` bypasses provider resolution and fetches the archive directly, which is what makes a
    /// vendor page or an already-downloaded `file://` archive installable — and what lets this
    /// path be exercised without the network in tests.
    async fn install(
        &self,
        spec: &Spec,
        url: Option<&str>,
        fetcher: &ReqwestFetcher,
        session: &Session,
        force: bool,
    ) -> Result<()> {
        let name = spec.install_name();
        Self::validate_name(&name)?;
        let directory = self.dir();
        let destination = directory.join(&name);
        if std::fs::symlink_metadata(&destination).is_ok() && !force {
            session
                .shell()
                .status(Verb::Fresh, format_args!("{name} is already installed"));
            return Ok(());
        }
        let release = if let Some(url) = url {
            Release {
                version: spec.version.label(),
                url: url.to_owned(),
                provider: "url",
            }
        } else {
            session
                .shell()
                .status(Verb::Resolving, format_args!("{name}"));
            Provider::for_distribution(&spec.distribution)
                .resolve(spec, fetcher)
                .await?
        };

        // One buffered download, through the crate's one fetch gate: `--offline` refuses here
        // before a byte moves, and a transient failure inherits the fetcher's retry schedule.
        let report = session
            .progress()
            .begin(Activity::Fetch, format!("{name} {}", release.version));
        let locator = ExternalLocator::new(release.url.clone());
        let limit = usize::try_from(Self::MAX_ARCHIVE_BYTES).unwrap_or(usize::MAX);
        let bytes = match Fetch::bounded(fetcher, &locator, limit, &report).await {
            Ok(bytes) => {
                report.finish(Outcome::Completed);
                bytes
            }
            Err(error) => {
                report.finish(Outcome::Failed);
                return Err(anyhow!("{error}"))
                    .with_context(|| format!("downloading `{}`", release.url));
            }
        };

        // A staging directory beside the install keeps the final move a same-filesystem rename and
        // means an interrupted extraction leaves no half-a-JDK under a real name.
        std::fs::create_dir_all(&directory)
            .with_context(|| format!("creating {}", directory.display()))?;
        let scratch = directory.join(format!(".tmp-{name}-{}", std::process::id()));
        if scratch.exists() {
            std::fs::remove_dir_all(&scratch)
                .with_context(|| format!("clearing {}", scratch.display()))?;
        }
        let extract = session.progress().begin(Activity::Extract, name.clone());
        let scratch_for_extract = scratch.clone();
        let unpacked = on_blocking_pool(move || {
            Archive::sniff(&bytes).and_then(|archive| archive.unpack(&bytes, &scratch_for_extract))
        })
        .await;
        let home = match unpacked {
            Ok(home) => {
                extract.finish(Outcome::Completed);
                home
            }
            Err(error) => {
                extract.finish(Outcome::Failed);
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(error).with_context(|| format!("extracting `{name}`"));
            }
        };

        let destination_for_publish = destination.clone();
        let scratch_for_publish = scratch.clone();
        on_blocking_pool(move || {
            Self::publish(&home, &destination_for_publish, &scratch_for_publish)
        })
        .await
        .with_context(|| format!("publishing `{name}`"))?;

        let metadata = serde_json::json!({
            "distribution": spec.distribution,
            "version": spec.version.label(),
            "release": release.version,
            "provider": release.provider,
            "url": release.url,
        });
        let body = serde_json::to_vec_pretty(&metadata).context("rendering toolchain metadata")?;
        let metadata_path = destination.join(Self::METADATA_FILE);
        on_blocking_pool(move || std::fs::write(&metadata_path, body))
            .await
            .with_context(|| format!("writing metadata for `{name}`"))?;

        session.shell().status(
            Verb::Installed,
            format_args!("{name} ({}) in {}", release.version, destination.display()),
        );
        Ok(())
    }

    /// Move an extracted home into place and drop the staging directory.
    fn publish(home: &Path, destination: &Path, scratch: &Path) -> io::Result<()> {
        if std::fs::symlink_metadata(destination).is_ok() {
            Self::remove_path(destination)?;
        }
        std::fs::rename(home, destination)?;
        let _ = std::fs::remove_dir_all(scratch);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_spec_forms() {
        let spec = Spec::parse("temurin@21").unwrap();
        assert_eq!(spec.distribution, "temurin");
        assert_eq!(spec.version, Version::Major(21));
        assert_eq!(spec.install_name(), "temurin-21");

        // A bare version defaults to Temurin; aliases collapse onto it.
        assert_eq!(Spec::parse("21").unwrap().distribution, "temurin");
        assert_eq!(Spec::parse("adoptium@21").unwrap(), spec);
        assert_eq!(Spec::parse("Temurin@21").unwrap(), spec);

        // Another vendor, an alias, and an exact release.
        assert_eq!(Spec::parse("zulu@lts").unwrap().distribution, "zulu");
        assert_eq!(
            Spec::parse("temurin@latest").unwrap().version,
            Version::Latest
        );
        let exact = Spec::parse("temurin@21.0.12.1+1").unwrap();
        assert_eq!(exact.version, Version::Exact("21.0.12.1+1".to_owned()));
        assert_eq!(exact.major(), Some(21));
        assert_eq!(exact.install_name(), "temurin-21.0.12.1+1");

        // An install name parses like the spec it came from...
        assert_eq!(Spec::parse("temurin-21").unwrap(), spec);
        assert_eq!(
            Spec::parse("zulu-17.0.13").unwrap(),
            Spec::parse("zulu@17.0.13").unwrap()
        );
        // ...while an exact version with a build suffix stays whole.
        assert_eq!(
            Spec::parse("8u422-b05").unwrap().version,
            Version::Exact("8u422-b05".to_owned())
        );
    }

    #[test]
    fn rejects_malformed_specs() {
        for raw in [
            "",
            "@",
            "temurin@",
            "temurin@two",
            "te/murin@21",
            "temurin@21 22",
            "temurin@21@22",
        ] {
            assert!(Spec::parse(raw).is_err(), "`{raw}` must not parse");
        }
    }

    #[test]
    fn classifies_release_versions() {
        assert_eq!(Version::major_of("21.0.12.1+1"), Some(21));
        assert_eq!(Version::major_of("8u422-b05"), Some(8));
        assert_eq!(Version::major_of("jdk-17.0.9"), Some(17));
        assert_eq!(Version::major_of("no-digits"), None);
    }

    #[test]
    fn a_manifest_selector_needs_a_version_for_downloading() {
        assert_eq!(
            Spec::from_selector(Some("temurin"), Some(21)).unwrap(),
            Spec::parse("temurin@21").unwrap()
        );
        assert_eq!(
            Spec::from_selector(None, Some(17)).unwrap().distribution,
            "temurin"
        );
        assert!(Spec::from_selector(Some("temurin"), None).is_err());
    }

    #[test]
    fn install_names_must_stay_direct_children_of_the_store() {
        for name in ["", ".", "..", "default", "a/b", "a\\b"] {
            assert!(
                Store::validate_name(name).is_err(),
                "`{name}` must be refused"
            );
        }
        for name in ["temurin-21", "my-jdk", "zulu-17.0.13"] {
            assert!(
                Store::validate_name(name).is_ok(),
                "`{name}` must be allowed"
            );
        }
    }
}
