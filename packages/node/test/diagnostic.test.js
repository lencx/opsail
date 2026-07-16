import assert from "node:assert/strict";
import {
  existsSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { createOpsail, resolveHardTimeoutMs } from "../src/client.js";
import { OpsailError } from "../src/errors.js";

const unsupported = process.platform === "win32";

test("the default process timeout leaves native cleanup grace", () => {
  assert.equal(resolveHardTimeoutMs(undefined, undefined), 30_000);
  assert.equal(resolveHardTimeoutMs(undefined, 20_000), 30_000);
  assert.equal(resolveHardTimeoutMs(undefined, 30_000), 40_000);
  assert.equal(resolveHardTimeoutMs(undefined, 60_000), 70_000);
  assert.equal(resolveHardTimeoutMs(undefined, 0), 30_000);
});

test("an explicit process timeout remains authoritative", () => {
  assert.equal(resolveHardTimeoutMs(2_000, 60_000), 2_000);
});

test(
  "owned Chrome requests receive an isolated temporary root that is removed after exit",
  { skip: unsupported },
  async (context) => {
    const response = JSON.stringify({
      protocolVersion: 1,
      ok: true,
      engine: { name: "opsail", version: "test" },
      result: {
        schemaVersion: 1,
        content: "test",
        contentHtml: "<p>test</p>",
        metadata: { title: "fixture" },
        source: {
          kind: "chrome",
          requested: "https://example.test/article",
          charset: "utf-8",
          bytes: 11,
        },
        extraction: { method: "readability", durationMs: 0 },
        quality: {
          grade: "good",
          contentCharacters: 4,
          wordCount: 1,
          extractionRatio: 1,
          probablyReadable: true,
        },
        warnings: [],
      },
    });
    const binaryPath = fakeBinary(context, "");
    const markerPath = path.join(path.dirname(binaryPath), "chrome-root.json");
    writeFileSync(
      binaryPath,
      `#!${process.execPath}
const fs = require("node:fs");
const path = require("node:path");
let input = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => { input += chunk; });
process.stdin.on("end", () => {
  const tempRoot = process.env.OPSAIL_CHROME_TEMP_ROOT;
  if (tempRoot !== undefined) {
    fs.writeFileSync(path.join(tempRoot, "fixture.txt"), "owned Chrome");
  }
  fs.writeFileSync(${JSON.stringify(markerPath)}, JSON.stringify({
    tempRoot: tempRoot ?? null,
    sourceKind: JSON.parse(input).source.kind,
  }));
  process.stdout.write(${JSON.stringify(response)});
});
`,
      { mode: 0o755 },
    );
    const client = createOpsail({ binaryPath });

    const result = await client.read({
      source: { kind: "chrome", url: "https://example.test/article" },
    });
    const observed = JSON.parse(readFileSync(markerPath, "utf8"));

    assert.equal(result.source.kind, "chrome");
    assert.equal(observed.sourceKind, "chrome");
    assert.equal(typeof observed.tempRoot, "string");
    assert.match(path.basename(observed.tempRoot), /^opsail-node-chrome-/u);
    assert.equal(existsSync(observed.tempRoot), false);
  },
);

test(
  "the client preserves stderr when the native protocol response is invalid",
  { skip: unsupported },
  async (context) => {
    const binaryPath = fakeBinary(
      context,
      'process.stderr.write("native detail\\n"); process.stdout.write("{");',
    );
    const client = createOpsail({ binaryPath });

    await assert.rejects(
      client.read({ source: { kind: "html", html: "<p>test</p>" } }),
      (error) => {
        assert(error instanceof OpsailError);
        assert.equal(error.code, "invalid-response");
        assert.equal(error.diagnostic, "native detail");
        return true;
      },
    );
  },
);

test(
  "the client preserves stderr captured before a process timeout",
  { skip: unsupported },
  async (context) => {
    const binaryPath = fakeBinary(
      context,
      'process.on("SIGTERM", () => { process.stderr.write("timeout detail\\n"); process.exit(0); }); setInterval(() => {}, 1_000);',
    );
    const client = createOpsail({ binaryPath, hardTimeoutMs: 2_000 });

    await assert.rejects(
      client.read({ source: { kind: "html", html: "<p>test</p>" } }),
      (error) => {
        assert(error instanceof OpsailError);
        assert.equal(error.code, "process-timeout");
        assert.equal(error.diagnostic, "timeout detail");
        return true;
      },
    );
  },
);

test(
  "the client includes the stderr chunk that crosses the output limit",
  { skip: unsupported },
  async (context) => {
    const binaryPath = fakeBinary(
      context,
      'process.stderr.write("crossing stderr detail\\n"); setInterval(() => {}, 1_000);',
    );
    const client = createOpsail({
      binaryPath,
      hardTimeoutMs: 5_000,
      maxOutputBytes: 8,
    });

    await assert.rejects(
      client.read({ source: { kind: "html", html: "<p>test</p>" } }),
      (error) => {
        assert(error instanceof OpsailError);
        assert.equal(error.code, "output-limit-exceeded");
        assert.equal(error.diagnostic, "crossing stderr detail");
        return true;
      },
    );
  },
);

test(
  "forced termination settles within the kill grace bound and removes descendants",
  { skip: unsupported, timeout: 12_000 },
  async (context) => {
    const binaryPath = fakeBinary(context, "");
    const markerPath = path.join(path.dirname(binaryPath), "processes.json");
    writeFileSync(
      binaryPath,
      `#!${process.execPath}
const { spawn } = require("node:child_process");
const fs = require("node:fs");
const grandchild = spawn(
  process.execPath,
  ["-e", "setInterval(() => {}, 1_000)"],
  { stdio: "ignore" },
);
grandchild.unref();
fs.writeFileSync(${JSON.stringify(markerPath)}, JSON.stringify({
  parentPid: process.pid,
  grandchildPid: grandchild.pid,
}));
process.on("SIGTERM", () => {});
process.stderr.write("too much output\\n");
setInterval(() => {}, 1_000);
`,
      { mode: 0o755 },
    );
    const client = createOpsail({
      binaryPath,
      hardTimeoutMs: 5_000,
      maxOutputBytes: 4,
    });
    const started = Date.now();
    let processes;

    try {
      await assert.rejects(
        client.read({ source: { kind: "html", html: "<p>test</p>" } }),
        (error) =>
          error instanceof OpsailError &&
          error.code === "output-limit-exceeded",
      );
      processes = JSON.parse(readFileSync(markerPath, "utf8"));
      assert(Date.now() - started < 7_000);
      assert.equal(
        await waitForProcessesToStop(Object.values(processes), 2_000),
        true,
        `fixture process group still contains ${JSON.stringify(processes)}`,
      );
    } finally {
      if (
        processes !== undefined &&
        Object.values(processes).some(isProcessRunning)
      ) {
        killFixtureProcessGroup(processes);
      }
    }
  },
);

async function waitForProcessesToStop(pids, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  do {
    if (pids.every((pid) => !isProcessRunning(pid))) return true;
    await new Promise((resolve) => setTimeout(resolve, 25));
  } while (Date.now() < deadline);
  return pids.every((pid) => !isProcessRunning(pid));
}

function isProcessRunning(pid) {
  try {
    process.kill(pid, 0);
  } catch (error) {
    if (error?.code === "ESRCH") return false;
    if (error?.code === "EPERM") return true;
    throw error;
  }

  if (process.platform === "linux") {
    try {
      const stat = readFileSync(`/proc/${pid}/stat`, "utf8");
      const state = stat.slice(stat.lastIndexOf(")") + 2).split(" ", 1)[0];
      return state !== "Z";
    } catch (error) {
      if (error?.code === "ENOENT") return false;
      throw error;
    }
  }

  return true;
}

function killFixtureProcessGroup({ parentPid, grandchildPid }) {
  try {
    process.kill(-parentPid, "SIGKILL");
    return;
  } catch (error) {
    if (error?.code !== "ESRCH") throw error;
  }
  for (const pid of [parentPid, grandchildPid]) {
    try {
      process.kill(pid, "SIGKILL");
    } catch (error) {
      if (error?.code !== "ESRCH") throw error;
    }
  }
}

function fakeBinary(context, source) {
  const directory = mkdtempSync(path.join(os.tmpdir(), "opsail-node-test-"));
  context.after(() => rmSync(directory, { recursive: true, force: true }));
  const binaryPath = path.join(directory, "opsail");
  writeFileSync(binaryPath, `#!${process.execPath}\n${source}\n`, { mode: 0o755 });
  return binaryPath;
}
