// Downloads and vendors the prebuilt `jals` binary published by
// `.github/workflows/release.yml`, so `jals` can be installed through bun/npm
// without a Rust toolchain. Zero runtime dependencies — node builtins only.
//
// `ensureBinary()` is the guaranteed entry point: `../bin/jals.mjs` calls it on
// every launch. It is a no-op once the binary is vendored and downloads it on
// the first run. Bun blocks lifecycle scripts (`postinstall`) by default, so the
// launcher — not `postinstall` — is what actually fetches the binary. Running
// this module directly (the `postinstall` optimisation) fetches eagerly but
// never fails the install: any error is downgraded to a warning and the launcher
// retries on first use.

import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath, pathToFileURL } from "node:url";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  renameSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";

const HERE = dirname(fileURLToPath(import.meta.url)); // <pkg>/npm/lib
const PKG_ROOT = join(HERE, "..", ".."); // <pkg>
const VENDOR_DIR = join(HERE, "..", "vendor"); // <pkg>/npm/vendor

const REPO = "https://github.com/topi-banana/jals";

// `${process.platform} ${process.arch}` -> Rust target + archive format. The
// six entries mirror the matrix in `.github/workflows/release.yml`.
const TARGETS = {
  "linux x64": { target: "x86_64-unknown-linux-gnu", format: "tar.gz" },
  "linux arm64": { target: "aarch64-unknown-linux-gnu", format: "tar.gz" },
  "darwin x64": { target: "x86_64-apple-darwin", format: "tar.gz" },
  "darwin arm64": { target: "aarch64-apple-darwin", format: "tar.gz" },
  "win32 x64": { target: "x86_64-pc-windows-msvc", format: "zip" },
  "win32 arm64": { target: "aarch64-pc-windows-msvc", format: "zip" },
};

function binName() {
  return process.platform === "win32" ? "jals.exe" : "jals";
}

export function vendoredBinary() {
  return join(VENDOR_DIR, binName());
}

function readVersion() {
  const pkg = JSON.parse(readFileSync(join(PKG_ROOT, "package.json"), "utf8"));
  return pkg.version;
}

function resolveTarget() {
  const key = `${process.platform} ${process.arch}`;
  const hit = TARGETS[key];
  if (!hit) {
    throw new Error(
      `no prebuilt jals binary for ${key}. ` +
        `Install from source instead: cargo install --git ${REPO} jals-cli`,
    );
  }
  return hit;
}

function assetBaseUrl(version) {
  // Override for mirrors / offline tests. Points at the directory holding the
  // release assets; a trailing slash is tolerated.
  const override = process.env.JALS_INSTALL_BASE_URL;
  if (override) return override.replace(/\/$/, "");
  return `${REPO}/releases/download/v${version}`;
}

async function fetchBuffer(url) {
  const res = await fetch(url, { redirect: "follow" });
  if (!res.ok) {
    throw new Error(`GET ${url} -> HTTP ${res.status} ${res.statusText}`);
  }
  return Buffer.from(await res.arrayBuffer());
}

function parseSha256(text) {
  // taiki-e/upload-rust-binary-action writes "<hex>  <filename>".
  return text.trim().split(/\s+/)[0].toLowerCase();
}

function extract(archivePath, format, destDir) {
  // System `tar` handles both `.tar.gz` and `.zip` (bsdtar on Windows/macOS).
  // `.zip` is only ever produced for the Windows targets, where `tar` is
  // bsdtar, so this stays dependency-free on every platform.
  const args =
    format === "zip"
      ? ["-xf", archivePath, "-C", destDir]
      : ["-xzf", archivePath, "-C", destDir];
  const res = spawnSync("tar", args, { stdio: "inherit" });
  if (res.error) throw res.error;
  if (res.status !== 0) {
    throw new Error(`tar exited with ${res.status} extracting ${archivePath}`);
  }
}

async function download() {
  const version = readVersion();
  const { target, format } = resolveTarget();
  const archiveName = `jals-v${version}-${target}.${format}`;
  const archiveUrl = `${assetBaseUrl(version)}/${archiveName}`;

  const [archive, sumText] = await Promise.all([
    fetchBuffer(archiveUrl),
    fetchBuffer(`${archiveUrl}.sha256`).then((b) => b.toString("utf8")),
  ]);

  const want = parseSha256(sumText);
  const got = createHash("sha256").update(archive).digest("hex");
  if (want !== got) {
    throw new Error(
      `checksum mismatch for ${archiveName}: expected ${want}, got ${got}`,
    );
  }

  const staging = mkdtempSync(join(tmpdir(), "jals-install-"));
  try {
    const archivePath = join(staging, archiveName);
    writeFileSync(archivePath, archive);
    extract(archivePath, format, staging);

    const extracted = join(staging, binName());
    if (!existsSync(extracted)) {
      throw new Error(`archive ${archiveName} did not contain ${binName()}`);
    }

    mkdirSync(VENDOR_DIR, { recursive: true });
    // Publish atomically: stage the bytes inside VENDOR_DIR (the OS tmpdir may
    // be a different filesystem, so a direct rename could hit EXDEV), then
    // rename into place. Concurrent first-runs each stage a pid-unique temp and
    // the final rename is last-writer-wins over identical content.
    const finalPath = vendoredBinary();
    const tmpFinal = join(VENDOR_DIR, `.${binName()}.tmp-${process.pid}`);
    writeFileSync(tmpFinal, readFileSync(extracted));
    if (process.platform !== "win32") chmodSync(tmpFinal, 0o755);
    renameSync(tmpFinal, finalPath);
    return finalPath;
  } finally {
    rmSync(staging, { recursive: true, force: true });
  }
}

let inflight = null;

export async function ensureBinary() {
  const path = vendoredBinary();
  if (existsSync(path)) return path;
  // Dedupe concurrent calls within this process; cross-process races are made
  // safe by the atomic rename in download().
  if (!inflight) inflight = download();
  return inflight;
}

// `postinstall` optimisation: fetch eagerly, but never fail the install — the
// launcher retries on first use if this is skipped (bun) or errors.
if (import.meta.url === pathToFileURL(process.argv[1] ?? "").href) {
  ensureBinary().then(
    (p) => console.error(`jals: vendored prebuilt binary at ${p}`),
    (err) =>
      console.error(
        `jals: prebuilt download deferred to first run (${err.message})`,
      ),
  );
}
