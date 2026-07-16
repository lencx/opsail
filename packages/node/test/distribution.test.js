import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdir, mkdtemp, rm } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { buildPlatformPackage } from "../scripts/build-platform-package.js";
import { platformTarget } from "../src/platforms.js";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const repositoryRoot = fileURLToPath(new URL("../../..", import.meta.url));
const executable = process.platform === "win32" ? "opsail.exe" : "opsail";
const binaryPath = path.join(repositoryRoot, "target", "debug", executable);
const npmCommand = process.platform === "win32" ? "npm.cmd" : "npm";

test("packed packages resolve and run the native binary without an override", async () => {
  const target = platformTarget(process.platform, process.arch);
  assert(target, `test runner ${process.platform}/${process.arch} must be supported`);

  const temporaryRoot = await mkdtemp(
    path.join(os.tmpdir(), "opsail-distribution-"),
  );
  const platformRoot = path.join(temporaryRoot, "platform");
  const tarballRoot = path.join(temporaryRoot, "tarballs");
  const installRoot = path.join(temporaryRoot, "install");
  const npmCache = path.join(temporaryRoot, "npm-cache");
  const env = { ...process.env, npm_config_cache: npmCache };
  delete env.OPSAIL_BINARY_PATH;

  try {
    await Promise.all([
      mkdir(tarballRoot, { recursive: true }),
      mkdir(installRoot, { recursive: true }),
    ]);
    await buildPlatformPackage({
      rustTarget: target.rustTarget,
      binaryPath,
      outputPath: platformRoot,
    });

    const platformTarball = await pack(platformRoot, tarballRoot, env);
    const facadeTarball = await pack(packageRoot, tarballRoot, env);
    await run(
      npmCommand,
      [
        "install",
        "--offline",
        "--omit=optional",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        facadeTarball,
        platformTarball,
      ],
      { cwd: installRoot, env },
    );

    const resolved = await run(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        'import { opsailPath } from "opsail"; process.stdout.write(opsailPath());',
      ],
      { cwd: installRoot, env },
    );
    assert.match(
      resolved.stdout,
      new RegExp(
        `node_modules[\\\\/]@opsail[\\\\/]${target.packageName.split("/").at(-1)}[\\\\/]bin[\\\\/]${executable}$`,
      ),
    );

    const cli = await run(
      process.execPath,
      [path.join(installRoot, "node_modules", "opsail", "bin", "opsail.js"), "--version"],
      { cwd: installRoot, env },
    );
    assert.match(cli.stdout, /^opsail \d+\.\d+\.\d+\s*$/);
    assert.equal(cli.stderr, "");

    const api = await run(
      process.execPath,
      [
        "--input-type=module",
        "--eval",
        'import { read } from "opsail"; const words = Array.from({ length: 140 }, (_, index) => `word${index}`).join(" "); const result = await read({ source: { kind: "html", html: `<!doctype html><html><head><title>Installed package</title></head><body><main><article><p>${words}</p></article></main></body></html>`, baseUrl: "https://example.test/article" } }); process.stdout.write(result.metadata.title);',
      ],
      { cwd: installRoot, env },
    );
    assert.equal(api.stdout, "Installed package");
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
});

async function pack(sourcePath, destination, env) {
  const result = await run(
    npmCommand,
    ["pack", "--silent", sourcePath, "--pack-destination", destination],
    { cwd: repositoryRoot, env },
  );
  const filename = result.stdout.trim().split(/\r?\n/).at(-1);
  assert(filename, `npm pack did not report a tarball for ${sourcePath}`);
  return path.join(destination, filename);
}

function run(command, args, { cwd, env }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd,
      env,
      shell: false,
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
    const stdout = [];
    const stderr = [];
    child.stdout.on("data", (chunk) => stdout.push(chunk));
    child.stderr.on("data", (chunk) => stderr.push(chunk));
    child.once("error", reject);
    child.once("close", (code, signal) => {
      const output = {
        code,
        signal,
        stdout: Buffer.concat(stdout).toString("utf8"),
        stderr: Buffer.concat(stderr).toString("utf8"),
      };
      if (code === 0 && signal === null) {
        resolve(output);
        return;
      }
      reject(
        new Error(
          `${command} ${args.join(" ")} failed (${signal ?? code})\n${output.stderr}`,
        ),
      );
    });
  });
}
