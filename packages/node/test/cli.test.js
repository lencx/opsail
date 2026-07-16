import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import test from "node:test";
import { fileURLToPath } from "node:url";

const executable = process.platform === "win32" ? "opsail.exe" : "opsail";
const binaryPath = fileURLToPath(
  new URL(`../../../target/debug/${executable}`, import.meta.url),
);
const cliPath = fileURLToPath(new URL("../bin/opsail.js", import.meta.url));

test("the package CLI forwards arguments to the native binary", async () => {
  const result = await run(process.execPath, [cliPath, "--version"], {
    ...process.env,
    OPSAIL_BINARY_PATH: binaryPath,
  });

  assert.equal(result.code, 0);
  assert.match(result.stdout, /^opsail \d+\.\d+\.\d+/);
  assert.equal(result.stderr, "");
});

test("the package CLI preserves native usage exit codes", async () => {
  const result = await run(process.execPath, [cliPath, "--not-an-opsail-option"], {
    ...process.env,
    OPSAIL_BINARY_PATH: binaryPath,
  });

  assert.equal(result.code, 2);
  assert.match(result.stderr, /unexpected argument|Usage:/i);
});

function run(command, args, env) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      env,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", reject);
    child.once("close", (code) => {
      resolve({
        code,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      });
    });
  });
}
