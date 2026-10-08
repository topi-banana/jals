//! The JDK providers `jals toolchain` resolves through, and the platform mapping each one needs.
//!
//! Two providers sit behind one vocabulary. Temurin is served by the Adoptium API directly: its
//! binary endpoint answers with a redirect to the release asset, so the exact build is named by
//! the release list rather than guessed, and no third party sits between the request and the
//! vendor. Every other distribution — Corretto, Zulu, Liberica, `GraalVM`, `openjdk` — goes through
//! foojay's Disco API, which is one metadata vocabulary covering all of them and the reason
//! adding a vendor here is a name rather than an integration.
//!
//! Everything network-shaped goes through [`jals_classpath::Fetch::bounded`], so a provider
//! inherits the fetcher's [`NetworkPolicy`](jals_classpath::NetworkPolicy) (an `--offline` install
//! refuses before the first request) and its retry schedule. Metadata requests report into the
//! silent progress handle: a run's one real download is the archive, and a batch summary counting
//! a handful of JSON documents beside it would be noise dressed as news.

use std::fmt::Write as _;

use anyhow::{Context as _, Result, anyhow, bail};
use jals_classpath::{ExternalLocator, Fetch, ReqwestFetcher};
use jals_progress::{Activity, Outcome, Progress};

use super::{Spec, Version};

/// One downloadable JDK archive, as a provider resolved it.
pub(crate) struct Release {
    /// The exact release the provider selected (`21.0.12.1+1`), for naming it back to the user.
    pub(crate) version: String,
    /// The archive's location. Following redirects is the fetcher's job.
    pub(crate) url: String,
    /// Which provider answered, recorded in the install's metadata.
    pub(crate) provider: &'static str,
}

/// The provider a distribution resolves through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Provider {
    /// The Adoptium API, serving Temurin.
    Adoptium,
    /// The foojay Disco API, serving every other vendor.
    Foojay,
}

impl Provider {
    /// The provider a distribution name resolves through.
    ///
    /// `temurin` has a first-party API; everything else has foojay. The split is by vendor, not
    /// by capability: foojay also serves Temurin, and using it only for the vendors without an
    /// API of their own keeps the Temurin path independent of a third party.
    pub(crate) fn for_distribution(distribution: &str) -> Self {
        if distribution == "temurin" {
            Self::Adoptium
        } else {
            Self::Foojay
        }
    }

    /// Resolve `spec` to a concrete archive for this host.
    pub(crate) async fn resolve(self, spec: &Spec, fetcher: &ReqwestFetcher) -> Result<Release> {
        match self {
            Self::Adoptium => Self::adoptium_resolve(spec, fetcher).await,
            Self::Foojay => Self::foojay_resolve(spec, fetcher).await,
        }
    }

    /// The major versions `distribution` offers as GA builds, ascending.
    pub(crate) async fn majors(
        self,
        distribution: &str,
        fetcher: &ReqwestFetcher,
    ) -> Result<Vec<u32>> {
        match self {
            Self::Adoptium => Self::adoptium_majors(fetcher).await,
            Self::Foojay => Self::foojay_majors(distribution, fetcher).await,
        }
    }

    /// Fetch and parse one provider document.
    async fn json(fetcher: &ReqwestFetcher, url: &str) -> Result<serde_json::Value> {
        let locator = ExternalLocator::new(url);
        // Silent: the archive is the run's download. A metadata request that showed up in the
        // fetch batch would make a one-Download line read `3 files`, two of them smaller than the
        // status line reporting them.
        let report = Progress::SILENT.begin(Activity::Fetch, "toolchain metadata");
        let bytes = match Fetch::bounded(fetcher, &locator, Self::METADATA_MAX_BYTES, &report).await
        {
            Ok(bytes) => {
                report.finish(Outcome::Completed);
                bytes
            }
            Err(error) => {
                report.finish(Outcome::Failed);
                bail!("{error}");
            }
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing the provider response from `{url}`"))
    }

    /// The host's operating system, in the vocabulary the providers share.
    fn os() -> Result<&'static str> {
        if cfg!(target_os = "linux") {
            Ok("linux")
        } else if cfg!(target_os = "macos") {
            Ok("macos")
        } else if cfg!(target_os = "windows") {
            Ok("windows")
        } else {
            bail!("`jals toolchain` does not know this host's operating system")
        }
    }

    /// The host's architecture, in the vocabulary the providers share.
    fn arch() -> Result<&'static str> {
        if cfg!(target_arch = "x86_64") {
            Ok("x64")
        } else if cfg!(target_arch = "aarch64") {
            Ok("aarch64")
        } else {
            bail!(
                "`jals toolchain` does not know this host's architecture; download a JDK by hand \
                 and register it with `jals toolchain link`"
            )
        }
    }

    /// Adoptium spells macOS `mac`; every other OS agrees with [`os`](Self::os).
    fn adoptium_os() -> Result<&'static str> {
        Ok(match Self::os()? {
            "macos" => "mac",
            other => other,
        })
    }

    /// The C library a Linux package must be built against, or `None` elsewhere.
    ///
    /// foojay ships musl and glibc builds under one distribution and a bare query answers with
    /// whichever comes first (musl, in practice). Naming glibc is the right reading of "this
    /// host": a musl userland running a static jals can still exec a glibc JDK, and the reverse is
    /// not true. The other OSes have no such split and the parameter is not sent.
    fn libc() -> Option<&'static str> {
        cfg!(target_os = "linux").then_some("glibc")
    }

    /// Percent-encode one query value.
    ///
    /// The releases endpoint needs a range (`[21,22)`) and a build number (`21.0.12.1+1`) to
    /// survive as data, and a hand-written encoder is smaller than a dependency for two call
    /// sites. Unreserved characters per RFC 3986 pass through; everything else is `%XX`.
    fn encode(value: &str) -> String {
        let mut encoded = String::with_capacity(value.len());
        for byte in value.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                    encoded.push(char::from(byte));
                }
                _ => {
                    let _ = write!(encoded, "%{byte:02X}");
                }
            }
        }
        encoded
    }

    /// The first few child releases, for an error that says what *is* available.
    fn preview(releases: &[String]) -> String {
        if releases.is_empty() {
            "none".to_owned()
        } else {
            releases
                .iter()
                .take(5)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        }
    }

    /// The first few packages' Java versions, for the same reason as [`preview`](Self::preview).
    fn package_preview(packages: &[serde_json::Value]) -> String {
        let versions: Vec<&str> = packages
            .iter()
            .take(5)
            .filter_map(|package| package["java_version"].as_str())
            .collect();
        if versions.is_empty() {
            "none".to_owned()
        } else {
            versions.join(", ")
        }
    }

    /// A release name with Adoptium's `jdk` prefix removed.
    ///
    /// Java 9+ is spelled `jdk-21.0.12.1+1` and Java 8 `jdk8u422-b05`, so both prefixes come off:
    /// an exact request spelled the version's own way has to match the release either way, and
    /// the version this reports back is the version, not the archive's name.
    fn release_version(release: &str) -> &str {
        release
            .strip_prefix("jdk-")
            .or_else(|| release.strip_prefix("jdk"))
            .unwrap_or(release)
    }
}

/// Adoptium (`api.adoptium.net`), serving Temurin.
impl Provider {
    /// Resolve through the Adoptium API.
    async fn adoptium_resolve(spec: &Spec, fetcher: &ReqwestFetcher) -> Result<Release> {
        let major = match &spec.version {
            Version::Lts => Self::adoptium_major(fetcher, "most_recent_lts").await?,
            Version::Latest => Self::adoptium_major(fetcher, "most_recent_feature_release").await?,
            Version::Major(major) => *major,
            Version::Exact(version) => Version::major_of(version)
                .ok_or_else(|| anyhow!("`{version}` does not name a Java major version"))?,
        };
        let releases = Self::adoptium_release_names(major, fetcher).await?;
        let release = match &spec.version {
            Version::Exact(want) => releases
                .iter()
                .find(|release| Self::release_version(release) == *want)
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "temurin has no GA release `{want}` (available for java {major}: {})",
                        Self::preview(&releases)
                    )
                })?,
            _ => releases
                .first()
                .cloned()
                .ok_or_else(|| anyhow!("temurin has no GA release for java {major}"))?,
        };
        let version = Self::release_version(&release).to_owned();
        let url = format!(
            "https://api.adoptium.net/v3/binary/version/{}/{}/{}/jdk/hotspot/normal/eclipse",
            Self::encode(&release),
            Self::adoptium_os()?,
            Self::arch()?,
        );
        Ok(Release {
            version,
            url,
            provider: "adoptium",
        })
    }

    /// One integer out of Adoptium's `available_releases` document.
    async fn adoptium_major(fetcher: &ReqwestFetcher, key: &str) -> Result<u32> {
        let value = Self::json(
            fetcher,
            "https://api.adoptium.net/v3/info/available_releases",
        )
        .await?;
        value[key]
            .as_u64()
            .and_then(|major| u32::try_from(major).ok())
            .ok_or_else(|| anyhow!("adoptium's `available_releases` did not name `{key}`"))
    }

    /// Adoptium's GA release names for `major`, newest first.
    async fn adoptium_release_names(major: u32, fetcher: &ReqwestFetcher) -> Result<Vec<String>> {
        let upper = major.saturating_add(1);
        let url = format!(
            "https://api.adoptium.net/v3/info/release_names?version=%5B{major}%2C{upper}%29\
             &architecture={}&image_type=jdk&os={}&release_type=ga&page_size=200&sort_order=DESC",
            Self::arch()?,
            Self::adoptium_os()?,
        );
        let value = Self::json(fetcher, &url).await?;
        Ok(value["releases"]
            .as_array()
            .map(|releases| {
                releases
                    .iter()
                    .filter_map(|release| release.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Adoptium's available GA major versions.
    async fn adoptium_majors(fetcher: &ReqwestFetcher) -> Result<Vec<u32>> {
        let value = Self::json(
            fetcher,
            "https://api.adoptium.net/v3/info/available_releases",
        )
        .await?;
        let mut majors: Vec<u32> = value["available_releases"]
            .as_array()
            .map(|releases| {
                releases
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .filter_map(|major| u32::try_from(major).ok())
                    .collect()
            })
            .unwrap_or_default();
        majors.sort_unstable();
        Ok(majors)
    }
}

/// foojay Disco (`api.foojay.io`), serving every vendor without an API of its own.
impl Provider {
    /// Resolve through foojay's package search.
    async fn foojay_resolve(spec: &Spec, fetcher: &ReqwestFetcher) -> Result<Release> {
        // The query is built as owned pairs because two of the values are formatted; the order is
        // stable so a provider error names a URL the same way every run.
        let mut params: Vec<(&str, String)> = vec![
            ("distro", spec.distribution.clone()),
            ("operating_system", Self::os()?.to_owned()),
            ("architecture", Self::arch()?.to_owned()),
        ];
        if let Some(libc) = Self::libc() {
            params.push(("lib_c_type", libc.to_owned()));
        }
        match &spec.version {
            Version::Lts => {
                params.push(("term_of_support", "lts".to_owned()));
                params.push(("latest", "available".to_owned()));
            }
            Version::Latest => params.push(("latest", "available".to_owned())),
            Version::Major(major) => {
                params.push(("version", major.to_string()));
                params.push(("latest", "available".to_owned()));
            }
            Version::Exact(version) => {
                let major = Version::major_of(version)
                    .ok_or_else(|| anyhow!("`{version}` does not name a Java major version"))?;
                // No `latest=available`: an exact build is, by definition, not the latest one.
                params.push(("version", major.to_string()));
            }
        }
        let packages = Self::foojay_packages(fetcher, &params).await?;
        let picked = match &spec.version {
            Version::Exact(want) => packages
                .iter()
                .find(|package| {
                    package["java_version"].as_str() == Some(want.as_str())
                        || package["distribution_version"].as_str() == Some(want.as_str())
                })
                .ok_or_else(|| {
                    anyhow!(
                        "{} has no GA package `{want}` for this host (available: {})",
                        spec.distribution,
                        Self::package_preview(&packages)
                    )
                })?,
            _ => packages.first().ok_or_else(|| {
                anyhow!(
                    "{} offers no GA package for this host and selection",
                    spec.distribution
                )
            })?,
        };
        let version = picked["java_version"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        let url = picked["links"]["pkg_download_redirect"]
            .as_str()
            .ok_or_else(|| {
                anyhow!(
                    "foojay's package for {} carries no download link",
                    spec.distribution
                )
            })?
            .to_owned();
        Ok(Release {
            version,
            url,
            provider: "foojay",
        })
    }

    /// foojay's matching packages, in the order it answers with.
    async fn foojay_packages(
        fetcher: &ReqwestFetcher,
        params: &[(&str, String)],
    ) -> Result<Vec<serde_json::Value>> {
        let mut url = String::from("https://api.foojay.io/disco/v3.0/packages");
        // `javafx_bundled=false` keeps a JavaFX build from winning over the plain JDK the
        // `jdk` package type already asked for, and `directly_downloadable=true` guarantees the
        // `pkg_download_redirect` the caller is about to follow exists.
        url.push_str(
            "?package_type=jdk&release_status=ga&javafx_bundled=false&directly_downloadable=true",
        );
        for (key, value) in params {
            url.push('&');
            url.push_str(key);
            url.push('=');
            url.push_str(&Self::encode(value));
        }
        let value = Self::json(fetcher, &url).await?;
        Ok(value["result"].as_array().cloned().unwrap_or_default())
    }

    /// foojay's GA major versions for one distribution.
    async fn foojay_majors(distribution: &str, fetcher: &ReqwestFetcher) -> Result<Vec<u32>> {
        let url = format!(
            "https://api.foojay.io/disco/v3.0/distributions/{}",
            Self::encode(distribution)
        );
        let value = Self::json(fetcher, &url).await?;
        let Some(distribution) = value["result"].as_array().and_then(|result| result.first())
        else {
            bail!("foojay does not know a distribution `{distribution}`");
        };
        let mut majors: Vec<u32> = distribution["versions"]
            .as_array()
            .map(|versions| {
                versions
                    .iter()
                    .filter_map(|version| version.as_str())
                    // `-ea` builds are previews, not installable releases: `jals toolchain`
                    // resolves GA only, and listing what it cannot install would lie.
                    .filter(|version| !version.contains("-ea"))
                    .filter_map(Version::major_of)
                    .collect()
            })
            .unwrap_or_default();
        majors.sort_unstable();
        majors.dedup();
        Ok(majors)
    }
}

impl Provider {
    /// The most metadata this reads before calling a response unreasonable.
    ///
    /// Every response is a small JSON document; the ceiling exists so a misrouted URL answering
    /// with an archive cannot fill memory before the parser notices.
    const METADATA_MAX_BYTES: usize = 4 * 1024 * 1024;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adoptium prefixes a release name with `jdk-` (Java 9+) or `jdk` (Java 8); both strip back
    /// to the version an exact request spells.
    #[test]
    fn strips_both_adoptium_release_prefixes() {
        assert_eq!(Provider::release_version("jdk-21.0.12.1+1"), "21.0.12.1+1");
        assert_eq!(Provider::release_version("jdk8u422-b05"), "8u422-b05");
        assert_eq!(Provider::release_version("21.0.12.1+1"), "21.0.12.1+1");
    }
}
