#!/usr/bin/env node

import {
  chmod,
  copyFile,
  mkdir,
  readFile,
  stat,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { rustPlatformTarget } from "../src/platforms.js";

const packageRoot = fileURLToPath(new URL("..", import.meta.url));

export async function buildPlatformPackage({
  rustTarget,
  binaryPath,
  outputPath,
}) {
  const target = rustPlatformTarget(rustTarget);
  if (target === undefined) {
    throw new Error(`Unsupported Rust target: ${rustTarget}`);
  }
  if (typeof binaryPath !== "string" || binaryPath.length === 0) {
    throw new TypeError("binaryPath must be a non-empty string");
  }
  if (typeof outputPath !== "string" || outputPath.length === 0) {
    throw new TypeError("outputPath must be a non-empty string");
  }

  const source = await stat(binaryPath);
  if (!source.isFile()) {
    throw new Error(`Opsail binary is not a file: ${binaryPath}`);
  }

  const facade = JSON.parse(
    await readFile(path.join(packageRoot, "package.json"), "utf8"),
  );
  const manifest = {
    name: target.packageName,
    version: facade.version,
    description: `Opsail native binary for ${target.platform}/${target.arch}`,
    os: [target.platform],
    cpu: [target.arch],
    preferUnplugged: true,
    files: ["bin", "README.md", "LICENSE"],
    engines: { node: ">=20" },
    repository: facade.repository,
    homepage: facade.homepage,
    license: facade.license,
    publishConfig: facade.publishConfig,
  };

  await mkdir(path.dirname(outputPath), { recursive: true });
  await mkdir(outputPath);

  const outputBinary = path.join(
    outputPath,
    ...target.binarySubpath.split("/"),
  );
  await mkdir(path.dirname(outputBinary), { recursive: true });
  await copyFile(binaryPath, outputBinary);
  await chmod(outputBinary, 0o755);
  await copyFile(path.join(packageRoot, "LICENSE"), path.join(outputPath, "LICENSE"));
  await writeFile(
    path.join(outputPath, "package.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  await writeFile(
    path.join(outputPath, "README.md"),
    platformReadme(target.packageName),
  );

  return { outputPath, manifest, binaryPath: outputBinary };
}

function platformReadme(packageName) {
  return `# ${packageName}\n\nThis is an implementation package containing the native Opsail binary for one platform. It is installed automatically by the public \`opsail\` package and is not a separate API.\n\nInstall and use the public package instead:\n\n\`\`\`sh\nnpm install opsail\n\`\`\`\n`;
}

function parseArguments(args) {
  const values = {};
  for (let index = 0; index < args.length; index += 2) {
    const flag = args[index];
    const value = args[index + 1];
    if (!flag?.startsWith("--") || value === undefined) {
      throw new Error(
        "Usage: build-platform-package --target TARGET --binary PATH --output DIRECTORY",
      );
    }
    values[flag.slice(2)] = value;
  }
  return {
    rustTarget: values.target,
    binaryPath: values.binary,
    outputPath: values.output,
  };
}

if (
  process.argv[1] !== undefined &&
  fileURLToPath(import.meta.url) === path.resolve(process.argv[1])
) {
  try {
    await buildPlatformPackage(parseArguments(process.argv.slice(2)));
  } catch (error) {
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
