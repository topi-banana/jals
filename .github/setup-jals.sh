#!/usr/bin/env bash
# The whole body of the `Setup jals` action (`action.yml` at the repository root).
#
# It lives in a file rather than inline in the YAML because it is a real program — version
# resolution, checksum verification, a source fallback — and because a file can be shellchecked and
# read with syntax highlighting. `action.yml` invokes it as
# `bash "${GITHUB_ACTION_PATH}/.github/setup-jals.sh"`, so it runs from the checked-out action
# rather than from the consumer's workspace.
#
# Every input arrives as an `INPUT_*` environment variable; nothing is read from the command line.
set -euo pipefail

VERSION_INPUT="${INPUT_VERSION:-latest}"
REPOSITORY="${INPUT_REPOSITORY:-topi-banana/jals}"
FROM_SOURCE="${INPUT_FROM_SOURCE:-auto}"
BASE_URL="${INPUT_BASE_URL:-}"
CACHE="${INPUT_CACHE:-true}"

REPO_URL="https://github.com/${REPOSITORY}"

case "${FROM_SOURCE}" in
  auto | always | never) ;;
  *)
    echo "::error::from-source must be one of auto, always, never (got '${FROM_SOURCE}')"
    exit 1
    ;;
esac

log() { echo "==> $*"; }

fail() {
  echo "::error::$*"
  exit 1
}

emit() { echo "$1=$2" >>"${GITHUB_OUTPUT}"; }

# The Rust target triple for this runner, matching the six cells of `.github/workflows/release.yml`
# — which is what makes the asset name predictable without ever listing the release's contents.
resolve_target() {
  local os="${RUNNER_OS:-}" arch="${RUNNER_ARCH:-}"
  case "${os}/${arch}" in
    Linux/X64) echo "x86_64-unknown-linux-gnu" ;;
    Linux/ARM64) echo "aarch64-unknown-linux-gnu" ;;
    macOS/X64) echo "x86_64-apple-darwin" ;;
    macOS/ARM64) echo "aarch64-apple-darwin" ;;
    Windows/X64) echo "x86_64-pc-windows-msvc" ;;
    Windows/ARM64) echo "aarch64-pc-windows-msvc" ;;
    *) return 1 ;;
  esac
}

# `.zip` is produced for the Windows targets only; everywhere else it is `.tar.gz`. Both are
# unpacked with the system `tar` (bsdtar on Windows and macOS), so extraction needs no unzip.
archive_extension() {
  case "$1" in
    *-pc-windows-msvc) echo "zip" ;;
    *) echo "tar.gz" ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    fail "no sha256sum or shasum on this runner; cannot verify the download"
  fi
}

# A release version is `X.Y.Z…` with an optional `v`. Anything else — `main`, a tag that is not a
# release, a commit SHA — is a git ref, and a git ref has no release asset, so it is built from
# source whatever `from-source` says short of `never`.
is_release_version() {
  [[ "$1" =~ ^v?[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]
}

resolve_latest() {
  local api="https://api.github.com/repos/${REPOSITORY}/releases/latest"
  local auth=()
  [[ -n "${GH_TOKEN:-}" ]] && auth=(-H "Authorization: Bearer ${GH_TOKEN}")
  local body
  body=$(curl -fsSL "${auth[@]}" -H "Accept: application/vnd.github+json" "${api}" 2>/dev/null) || return 1
  # No `jq` guarantee on every runner image, so the one field is read with sed rather than a parser.
  sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' <<<"${body}" | head -n1
}

# ---------------------------------------------------------------------------------------------
# Resolve what to install.
# ---------------------------------------------------------------------------------------------

VERSION="${VERSION_INPUT}"
if [[ "${VERSION}" == "latest" ]]; then
  if resolved=$(resolve_latest) && [[ -n "${resolved}" ]]; then
    VERSION="${resolved}"
    log "resolved latest release to ${VERSION}"
  elif [[ "${FROM_SOURCE}" == "never" ]]; then
    fail "no published release found for ${REPOSITORY} and from-source is 'never'"
  else
    # A repository with no release yet still has a default branch, and building it is a better
    # answer than failing on a version that does not exist.
    VERSION="HEAD"
    log "no published release for ${REPOSITORY}; building the default branch from source"
  fi
fi

if is_release_version "${VERSION}"; then
  VERSION="${VERSION#v}"
  KIND="release"
else
  KIND="ref"
fi

if [[ "${KIND}" == "ref" && "${FROM_SOURCE}" == "never" ]]; then
  fail "version '${VERSION_INPUT}' is a git ref, which has no prebuilt asset, and from-source is 'never'"
fi

WANT_PREBUILT="false"
if [[ "${KIND}" == "release" && "${FROM_SOURCE}" != "always" ]]; then
  WANT_PREBUILT="true"
fi

BIN_NAME="jals"
[[ "${RUNNER_OS:-}" == "Windows" ]] && BIN_NAME="jals.exe"

# The install lives in the runner tool cache so a second job step — or a second job on a
# self-hosted runner — reuses it. The key carries how it was produced: a source build of `main` and
# a prebuilt `0.2.0` are different bytes and must never share a directory.
TOOL_ROOT="${RUNNER_TOOL_CACHE:-${RUNNER_TEMP:-/tmp}/jals-tool-cache}"
SLUG=$(printf '%s' "${VERSION}" | tr -c '[:alnum:]._-' '-')
KEY="${KIND}-${SLUG}"
[[ "${WANT_PREBUILT}" == "true" ]] || KEY="${KEY}-source"
INSTALL_DIR="${TOOL_ROOT}/jals/${KEY}/${RUNNER_ARCH:-unknown}"
BIN_DIR="${INSTALL_DIR}/bin"
BIN_PATH="${BIN_DIR}/${BIN_NAME}"

CACHE_HIT="false"
SOURCE="prebuilt"

if [[ "${CACHE}" == "true" && -x "${BIN_PATH}" ]]; then
  CACHE_HIT="true"
  log "tool cache already holds jals ${VERSION} at ${BIN_PATH}"
fi

# ---------------------------------------------------------------------------------------------
# Install.
# ---------------------------------------------------------------------------------------------

download_prebuilt() {
  local target extension archive url staging
  target=$(resolve_target) || {
    log "no prebuilt jals for ${RUNNER_OS:-?}/${RUNNER_ARCH:-?}"
    return 1
  }
  extension=$(archive_extension "${target}")
  archive="jals-v${VERSION}-${target}.${extension}"
  if [[ -n "${BASE_URL}" ]]; then
    url="${BASE_URL%/}/${archive}"
  else
    url="${REPO_URL}/releases/download/v${VERSION}/${archive}"
  fi

  staging=$(mktemp -d "${RUNNER_TEMP:-/tmp}/jals-install-XXXXXX")
  # shellcheck disable=SC2064 # the path is expanded now on purpose: the trap must not read a
  # variable a later call has reassigned.
  trap "rm -rf '${staging}'" RETURN

  log "downloading ${url}"
  curl -fsSL --retry 3 --retry-connrefused -o "${staging}/${archive}" "${url}" || {
    log "no release asset at ${url}"
    return 1
  }
  curl -fsSL --retry 3 --retry-connrefused -o "${staging}/${archive}.sha256" "${url}.sha256" ||
    fail "downloaded ${archive} but its ${archive}.sha256 is missing; refusing to install unverified bytes"

  local want got
  # `upload-rust-binary-action` writes `<hex>  <filename>`; only the digest is read, so the file
  # name recorded inside it never has to match where curl happened to put the bytes.
  want=$(tr -d '\r' <"${staging}/${archive}.sha256" | awk 'NR==1 {print tolower($1)}')
  got=$(sha256_of "${staging}/${archive}")
  [[ "${want}" == "${got}" ]] ||
    fail "checksum mismatch for ${archive}: expected ${want}, got ${got}"
  log "sha256 verified (${got})"

  tar -xf "${staging}/${archive}" -C "${staging}" ||
    fail "could not extract ${archive}"
  [[ -f "${staging}/${BIN_NAME}" ]] ||
    fail "${archive} did not contain ${BIN_NAME}"

  mkdir -p "${BIN_DIR}"
  # Staged inside the destination and renamed, so a concurrent install on the same self-hosted
  # runner never exposes a half-written executable — and never crosses a filesystem boundary the
  # way a rename out of the OS temp directory would.
  local tmp_final="${BIN_DIR}/.${BIN_NAME}.tmp-$$"
  cp "${staging}/${BIN_NAME}" "${tmp_final}"
  chmod +x "${tmp_final}"
  mv -f "${tmp_final}" "${BIN_PATH}"
}

build_from_source() {
  command -v cargo >/dev/null 2>&1 ||
    fail "cargo is not on PATH; add a Rust toolchain step (dtolnay/rust-toolchain@stable) before this action, or pin a released 'version'"

  local args=(install --locked --force --git "${REPO_URL}" --root "${INSTALL_DIR}")
  if [[ "${KIND}" == "release" ]]; then
    args+=(--tag "v${VERSION}")
  elif [[ "${VERSION}" != "HEAD" ]]; then
    # A ref is a branch, a tag or a commit and the caller does not say which. `--rev` accepts any
    # of the three (git resolves it), so one flag covers all of them.
    args+=(--rev "${VERSION}")
  fi
  # `jals-cli` is required, not optional: this is a workspace shipping several binaries, so
  # `cargo install --git` cannot pick one on its own.
  args+=(jals-cli)

  log "cargo ${args[*]}"
  cargo "${args[@]}" ||
    fail "cargo install failed for ${REPOSITORY} at ${VERSION}"
}

if [[ "${CACHE_HIT}" != "true" ]]; then
  rm -rf "${INSTALL_DIR}"
  if [[ "${WANT_PREBUILT}" == "true" ]] && download_prebuilt; then
    SOURCE="prebuilt"
  elif [[ "${FROM_SOURCE}" == "never" ]]; then
    fail "no prebuilt jals ${VERSION} for this runner and from-source is 'never'"
  else
    build_from_source
    SOURCE="source"
  fi
elif [[ "${WANT_PREBUILT}" != "true" ]]; then
  SOURCE="source"
fi

[[ -x "${BIN_PATH}" ]] || fail "jals was not installed at ${BIN_PATH}"

echo "${BIN_DIR}" >>"${GITHUB_PATH}"

emit version "${VERSION}"
emit path "${BIN_PATH}"
emit bin-dir "${BIN_DIR}"
emit source "${SOURCE}"
emit cache-hit "${CACHE_HIT}"

log "$("${BIN_PATH}" --version) installed from ${SOURCE} at ${BIN_PATH}"
