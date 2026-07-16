const targets = [
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
    arch: "x64",
    packageName: "@opsail/win32-x64",
    rustTarget: "x86_64-pc-windows-msvc",
    binarySubpath: "bin/opsail.exe",
    runner: "windows-2025",
    asset: "opsail-x86_64-pc-windows-msvc.zip",
  },
];

export const PLATFORM_TARGETS = Object.freeze(
  targets.map((target) => Object.freeze(target)),
);

export function platformTarget(platform, arch) {
  return PLATFORM_TARGETS.find(
    (target) => target.platform === platform && target.arch === arch,
  );
}

export function rustPlatformTarget(rustTarget) {
  return PLATFORM_TARGETS.find((target) => target.rustTarget === rustTarget);
}

export function releaseMatrix() {
  return {
    include: PLATFORM_TARGETS.map(
      ({ runner, rustTarget, asset, binarySubpath, packageName }) => ({
        runner,
        target: rustTarget,
        asset,
        binary: binarySubpath.split("/").at(-1),
        package: packageName,
      }),
    ),
  };
}
