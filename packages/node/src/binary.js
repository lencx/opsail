import { existsSync } from "node:fs";
import path from "node:path";
import { createRequire } from "node:module";

import { OpsailError } from "./errors.js";
import { platformTarget } from "./platforms.js";

const require = createRequire(import.meta.url);

export function opsailPath(options = {}) {
  if (options === null || typeof options !== "object" || Array.isArray(options)) {
    throw new TypeError("opsailPath options must be an object");
  }

  if (options.binaryPath !== undefined) {
    return configuredBinaryPath(options.binaryPath);
  }

  const environmentPath = process.env.OPSAIL_BINARY_PATH;
  if (environmentPath !== undefined && environmentPath.length > 0) {
    return configuredBinaryPath(environmentPath);
  }

  return resolvePlatformBinary();
}

export function resolvePlatformBinary(options = {}) {
  const platform = options.platform ?? process.platform;
  const arch = options.arch ?? process.arch;
  const target = platformTarget(platform, arch);
  if (target === undefined) {
    throw new OpsailError(
      `Opsail does not provide a binary for ${platform}/${arch}`,
      {
        code: "unsupported-platform",
        stage: "process",
        recovery:
          "Pass an absolute binaryPath or set OPSAIL_BINARY_PATH to a compatible Opsail binary.",
      },
    );
  }

  const resolve = options.resolve ?? require.resolve;
  let resolved;
  try {
    resolved = resolve(`${target.packageName}/${target.binarySubpath}`);
  } catch (cause) {
    throw new OpsailError(
      `The optional platform package ${target.packageName} is not installed`,
      {
        code: "platform-package-missing",
        stage: "process",
        recovery:
          "Reinstall opsail without omitting optional dependencies, or configure binaryPath explicitly.",
        cause,
      },
    );
  }

  return executablePath(resolved, options.exists ?? existsSync);
}

function configuredBinaryPath(candidate) {
  if (typeof candidate !== "string" || candidate.length === 0) {
    throw new OpsailError("Opsail binary path must be a non-empty string", {
      code: "invalid-binary-path",
      stage: "process",
    });
  }
  if (!path.isAbsolute(candidate)) {
    throw new OpsailError("Opsail binary path must be absolute", {
      code: "invalid-binary-path",
      stage: "process",
    });
  }
  return executablePath(candidate, existsSync);
}

function executablePath(candidate, exists) {
  if (!/\.asar(?=[\\/])/.test(candidate)) {
    return candidate;
  }

  const unpacked = candidate.replace(/\.asar(?=[\\/])/, ".asar.unpacked");
  if (exists(unpacked)) {
    return unpacked;
  }

  throw new OpsailError("Opsail binary is packed inside an Electron ASAR archive", {
    code: "binary-packed-in-asar",
    stage: "process",
    recovery:
      "Unpack node_modules/@opsail/**/bin/opsail* when packaging the Electron application.",
  });
}
