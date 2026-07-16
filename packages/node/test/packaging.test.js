import assert from "node:assert/strict";
import { mkdtemp, readFile, stat } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { buildPlatformPackage } from "../scripts/build-platform-package.js";
import { PLATFORM_TARGETS } from "../src/platforms.js";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));
const packageManifest = JSON.parse(
  await readFile(path.join(packageRoot, "package.json"), "utf8"),
);
const executable = process.platform === "win32" ? "opsail.exe" : "opsail";
const binaryPath = fileURLToPath(
  new URL(`../../../target/debug/${executable}`, import.meta.url),
);

test("the facade pins every platform package to its exact version", () => {
  assert.deepEqual(
    packageManifest.optionalDependencies,
    Object.fromEntries(
      PLATFORM_TARGETS.map(({ packageName }) => [
        packageName,
        packageManifest.version,
      ]),
    ),
  );
});

test("the platform package builder emits an implementation-only package", async () => {
  const target = PLATFORM_TARGETS.find(
    ({ platform, arch }) => platform === process.platform && arch === process.arch,
  );
  assert(target, `test runner ${process.platform}/${process.arch} must be supported`);

  const temporaryRoot = await mkdtemp(path.join(os.tmpdir(), "opsail-package-"));
  const outputPath = path.join(temporaryRoot, "platform");
  await buildPlatformPackage({
    rustTarget: target.rustTarget,
    binaryPath,
    outputPath,
  });

  const manifest = JSON.parse(
    await readFile(path.join(outputPath, "package.json"), "utf8"),
  );
  assert.equal(manifest.name, target.packageName);
  assert.equal(manifest.version, packageManifest.version);
  assert.deepEqual(manifest.os, [target.platform]);
  assert.deepEqual(manifest.cpu, [target.arch]);
  assert.equal(manifest.preferUnplugged, true);
  assert.equal(manifest.bin, undefined);
  assert.equal(manifest.main, undefined);
  assert.equal(manifest.exports, undefined);
  assert.equal(manifest.scripts, undefined);

  const copiedBinary = path.join(outputPath, ...target.binarySubpath.split("/"));
  assert.equal((await stat(copiedBinary)).isFile(), true);
  if (process.platform !== "win32") {
    assert.equal((await stat(copiedBinary)).mode & 0o111, 0o111);
  }

  const readme = await readFile(path.join(outputPath, "README.md"), "utf8");
  assert.match(readme, /implementation package/i);
  assert.match(readme, /npm install opsail/);
  assert.match(await readFile(path.join(outputPath, "LICENSE"), "utf8"), /Apache License/);
});
