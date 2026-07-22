import assert from "node:assert/strict";
import path from "node:path";
import test from "node:test";

import {
  PLATFORM_TARGETS,
  platformTarget,
} from "../src/platforms.js";
import {
  opsailPath,
  resolvePlatformBinary,
} from "../src/binary.js";
import { OpsailError } from "../src/errors.js";

const expectedTargets = [
  {
    platform: "darwin",
    arch: "arm64",
    packageName: "@opsail/darwin-arm64",
    rustTarget: "aarch64-apple-darwin",
    binarySubpath: "bin/opsail",
    runner: "macos-15",
    asset: "opsail-aarch64-apple-darwin.tar.gz",
  },
  {
    platform: "darwin",
    arch: "x64",
    packageName: "@opsail/darwin-x64",
    rustTarget: "x86_64-apple-darwin",
    binarySubpath: "bin/opsail",
    runner: "macos-15-intel",
    asset: "opsail-x86_64-apple-darwin.tar.gz",
  },
  {
    platform: "linux",
    arch: "arm64",
    packageName: "@opsail/linux-arm64",
    rustTarget: "aarch64-unknown-linux-musl",
    binarySubpath: "bin/opsail",
    runner: "ubuntu-24.04-arm",
    asset: "opsail-aarch64-unknown-linux-musl.tar.gz",
  },
  {
    platform: "linux",
    arch: "x64",
    packageName: "@opsail/linux-x64",
    rustTarget: "x86_64-unknown-linux-musl",
    binarySubpath: "bin/opsail",
    runner: "ubuntu-24.04",
    asset: "opsail-x86_64-unknown-linux-musl.tar.gz",
  },
  {
    platform: "win32",
    arch: "arm64",
    packageName: "@opsail/win32-arm64",
    rustTarget: "aarch64-pc-windows-msvc",
    binarySubpath: "bin/opsail.exe",
    runner: "windows-11-arm",
    asset: "opsail-aarch64-pc-windows-msvc.zip",
  },
  {
    platform: "win32",
    arch: "x64",
    packageName: "@opsail/win32-x64",
    rustTarget: "x86_64-pc-windows-msvc",
    binarySubpath: "bin/opsail.exe",
    runner: "windows-2025",
    asset: "opsail-x86_64-pc-windows-msvc.zip",
  },
];

test("platform mappings are closed and release-ready", () => {
  assert.deepEqual(PLATFORM_TARGETS, expectedTargets);
  for (const expected of expectedTargets) {
    assert.deepEqual(platformTarget(expected.platform, expected.arch), expected);
  }
  assert.equal(platformTarget("linux", "riscv64"), undefined);
  assert.equal(platformTarget("freebsd", "x64"), undefined);
});

test("the platform resolver selects the matching optional package", () => {
  const resolved = path.join(path.parse(process.cwd()).root, "packages", "opsail");
  let requested;

  assert.equal(
    resolvePlatformBinary({
      platform: "darwin",
      arch: "arm64",
      resolve(specifier) {
        requested = specifier;
        return resolved;
      },
    }),
    resolved,
  );
  assert.equal(requested, "@opsail/darwin-arm64/bin/opsail");
});

test("explicit configuration takes precedence over the environment", () => {
  const root = path.parse(process.cwd()).root;
  const explicit = path.join(root, "explicit", "opsail");
  const environment = path.join(root, "environment", "opsail");
  const previous = process.env.OPSAIL_BINARY_PATH;

  try {
    process.env.OPSAIL_BINARY_PATH = environment;
    assert.equal(opsailPath({ binaryPath: explicit }), explicit);
    assert.equal(opsailPath(), environment);
  } finally {
    if (previous === undefined) {
      delete process.env.OPSAIL_BINARY_PATH;
    } else {
      process.env.OPSAIL_BINARY_PATH = previous;
    }
  }
});

test("unsupported targets fail with a stable error", () => {
  assert.throws(
    () =>
      resolvePlatformBinary({
        platform: "freebsd",
        arch: "x64",
        resolve() {
          assert.fail("unsupported targets must not resolve a package");
        },
      }),
    (error) =>
      error instanceof OpsailError && error.code === "unsupported-platform",
  );
});

test("missing platform packages fail with a stable error", () => {
  const cause = Object.assign(new Error("not found"), { code: "MODULE_NOT_FOUND" });
  assert.throws(
    () =>
      resolvePlatformBinary({
        platform: "linux",
        arch: "x64",
        resolve() {
          throw cause;
        },
      }),
    (error) =>
      error instanceof OpsailError &&
      error.code === "platform-package-missing" &&
      error.cause === cause,
  );
});

test("ASAR binaries resolve through the unpacked archive", () => {
  const packed = path.join(
    path.parse(process.cwd()).root,
    "app",
    "resources",
    "app.asar",
    "node_modules",
    "@opsail",
    "linux-x64",
    "bin",
    "opsail",
  );
  const unpacked = packed.replace("app.asar", "app.asar.unpacked");

  assert.equal(
    resolvePlatformBinary({
      platform: "linux",
      arch: "x64",
      resolve: () => packed,
      exists: (candidate) => candidate === unpacked,
    }),
    unpacked,
  );

  assert.throws(
    () =>
      resolvePlatformBinary({
        platform: "linux",
        arch: "x64",
        resolve: () => packed,
        exists: () => false,
      }),
    (error) =>
      error instanceof OpsailError && error.code === "binary-packed-in-asar",
  );
});
