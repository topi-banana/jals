#!/usr/bin/env node
// Launcher that bun/npm links onto PATH as `jals`. It resolves (downloading on
// first use) the vendored prebuilt binary and execs it, forwarding args, stdio,
// exit code, and terminating signal.

import { spawnSync } from "node:child_process";
import { ensureBinary } from "../lib/install.mjs";

let bin;
try {
  bin = await ensureBinary();
} catch (err) {
  console.error(`jals: could not obtain the prebuilt binary.\n${err.message}`);
  process.exit(1);
}

const res = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
if (res.error) {
  console.error(`jals: failed to launch ${bin}: ${res.error.message}`);
  process.exit(1);
}
if (res.signal) {
  process.kill(process.pid, res.signal);
}
process.exit(res.status ?? 0);
