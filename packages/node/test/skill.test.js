import assert from "node:assert/strict";
import { access, readFile } from "node:fs/promises";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

const repositoryRoot = fileURLToPath(new URL("../../../", import.meta.url));
const bootstrapRoot = path.join(repositoryRoot, "skills", "bootstrap-opsail");
const bootstrapSkillPath = path.join(bootstrapRoot, "SKILL.md");
const shellInstallerPath = path.join(bootstrapRoot, "scripts", "install.sh");
const powerShellInstallerPath = path.join(
  bootstrapRoot,
  "scripts",
  "install.ps1",
);
const opsailSkillPath = path.join(
  repositoryRoot,
  "skills",
  "opsail",
  "SKILL.md",
);
const readmePath = path.join(repositoryRoot, "README.md");
const readmeZhPath = path.join(repositoryRoot, "README.zh-CN.md");
const releaseWorkflowPath = path.join(
  repositoryRoot,
  ".github",
  "workflows",
  "release.yml",
);
const packagePath = path.join(repositoryRoot, "packages", "node", "package.json");

function frontmatter(source) {
  const match = source.match(/^---\n([\s\S]*?)\n---(?:\n|$)/);
  assert(match, "SKILL.md must start with YAML frontmatter");
  return match[1];
}

function scalar(source, key) {
  const match = source.match(new RegExp(`^${key}: (.+)$`, "m"));
  assert(match, `SKILL.md frontmatter must define ${key}`);
  return match[1];
}

function topLevelKeys(source) {
  return frontmatter(source)
    .split("\n")
    .filter((line) => /^[a-z][a-z0-9-]*:/.test(line))
    .map((line) => line.slice(0, line.indexOf(":")));
}

function body(source) {
  const match = source.match(/^---\n[\s\S]*?\n---\n([\s\S]*)$/);
  assert(match, "SKILL.md must contain a Markdown body");
  return match[1];
}

function fencedCode(source) {
  return [...source.matchAll(/```[^\n]*\n([\s\S]*?)```/g)]
    .map((match) => match[1])
    .join("\n");
}

test("bootstrap is independently versioned and runtime matches the public package", async () => {
  const [bootstrapSkill, opsailSkill, packageSource] = await Promise.all([
    readFile(bootstrapSkillPath, "utf8"),
    readFile(opsailSkillPath, "utf8"),
    readFile(packagePath, "utf8"),
  ]);
  const manifest = JSON.parse(packageSource);
  const bootstrapMetadata = JSON.parse(
    scalar(frontmatter(bootstrapSkill), "metadata"),
  );
  const opsailMetadata = JSON.parse(
    scalar(frontmatter(opsailSkill), "metadata"),
  );
  const standardFields = [
    "name",
    "description",
    "license",
    "compatibility",
    "metadata",
  ];

  assert.deepEqual(topLevelKeys(bootstrapSkill), standardFields);
  assert.deepEqual(topLevelKeys(opsailSkill), standardFields);
  assert.equal(scalar(frontmatter(bootstrapSkill), "name"), "bootstrap-opsail");
  assert.equal(scalar(frontmatter(opsailSkill), "name"), "opsail");
  assert.match(
    bootstrapMetadata.version,
    /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)$/,
  );
  assert.equal(opsailMetadata.version, manifest.version);
  assert.match(
    scalar(frontmatter(opsailSkill), "compatibility"),
    new RegExp(`\\bOpsail ${manifest.version.replaceAll(".", "\\.")}\\b`),
  );
  assert.equal(bootstrapMetadata["opsail-role"], "transient-bootstrap");
  assert.equal(bootstrapMetadata.openclaw, undefined);
  assert.deepEqual(opsailMetadata.openclaw.requires.bins, ["opsail"]);
  assert.deepEqual(opsailMetadata.openclaw.install, [
    {
      id: "node",
      kind: "node",
      package: `opsail@${manifest.version}`,
      bins: ["opsail"],
      label: "Install Opsail (npm)",
    },
  ]);
  assert(opsailMetadata.hermes);
  assert.match(opsailSkill, /`read` extracts readable content/);
  assert.match(opsailSkill, /opsail refit codex doctor/);
  assert.match(opsailSkill, /opsail refit codex enable usage/);
  assert.match(opsailSkill, /opsail refit codex enable usage --launch/);
  assert.match(opsailSkill, /opsail refit codex disable usage/);
  assert.match(opsailSkill, /opsail refit codex update/);
  assert.match(opsailSkill, /--force/);
  assert.match(opsailSkill, /never quit, kill, restart, reload, modify, or re-sign ChatGPT/);
  assert.match(opsailSkill, /public default port is `55321`/);
  assert.doesNotMatch(opsailSkill, /repair the DOM adapter/i);
  assert.doesNotMatch(opsailSkill, /opsail-refit-codex-dom-adapter\.js/);
});

test("root README stays concise and release workflow keeps one installer source", async () => {
  const [readme, readmeZh, releaseWorkflow] = await Promise.all([
    readFile(readmePath, "utf8"),
    readFile(readmeZhPath, "utf8"),
    readFile(releaseWorkflowPath, "utf8"),
  ]);

  assert.match(readme, /## Core characteristics/);
  assert.match(
    readme,
    /See \[`opsail-read`\]\(https:\/\/github\.com\/lencx\/opsail\/blob\/main\/crates\/opsail-read\/README\.md\)/,
  );
  assert.match(
    readme,
    /See \[`opsail-refit-codex`\]\(https:\/\/github\.com\/lencx\/opsail\/blob\/main\/crates\/opsail-refit-codex\/README\.md\)/,
  );
  assert.match(readme, /\[GitHub Releases\]\(https:\/\/github\.com\/lencx\/opsail\/releases\/latest\)/);
  assert.match(
    readme,
    /\[`bootstrap-opsail` Skill\]\(https:\/\/github\.com\/lencx\/opsail\/blob\/main\/skills\/bootstrap-opsail\/SKILL\.md\)/,
  );
  for (const source of [readme, readmeZh]) {
    assert.doesNotMatch(source, /!?\[[^\]]*\]\((?!https:\/\/|#)[^)]+\)/);
    assert.doesNotMatch(source, /(?:href|src)="(?!https:\/\/|#)[^"]+"/);
    assert.doesNotMatch(source, /^\s*\[[^\]]+\]:\s*(?!https:\/\/|#)\S+/m);
  }
  assert.ok(readme.split("\n").length <= 100);
  assert.doesNotMatch(
    readme,
    /raw\.githubusercontent\.com\/lencx\/opsail\/(?:main|refs\/heads\/main)\/scripts\/install\.(?:sh|ps1)/,
  );
  assert.doesNotMatch(
    releaseWorkflow,
    /skills\/(?:bootstrap-opsail|opsail)\/SKILL\.md|dist\/opsail-(?:bootstrap-)?install\.(?:sh|ps1)/,
  );

  await Promise.all(
    ["install.sh", "install.ps1"].map((filename) =>
      assert.rejects(access(path.join(repositoryRoot, "scripts", filename)), {
        code: "ENOENT",
      }),
    ),
  );
});

test("bootstrap keeps only the required reconciliation contract", async () => {
  const [bootstrapSkill, shellInstaller, powerShellInstaller] =
    await Promise.all([
      readFile(bootstrapSkillPath, "utf8"),
      readFile(shellInstallerPath, "utf8"),
      readFile(powerShellInstallerPath, "utf8"),
    ]);
  const instructions = body(bootstrapSkill);
  const commands = fencedCode(bootstrapSkill);
  const compatibility = scalar(frontmatter(bootstrapSkill), "compatibility");

  assert.match(compatibility, /Node\.js is optional/);
  assert.match(
    instructions,
    /api\.github\.com\/repos\/lencx\/opsail\/releases\/latest/,
  );
  assert.match(instructions, /npm view opsail@latest version/);
  assert.match(instructions, /Node\.js 20\+/);
  assert.match(instructions, /Runtime Skill:[^\n]*Node\.js 18\+/);
  assert.match(instructions, /HERMES_HOME/);
  assert.doesNotMatch(instructions, /default profile only/);
  assert.match(instructions, /optionalDependencies --json/);
  assert.match(instructions, /PLATFORM_PACKAGE/);
  assert.doesNotMatch(instructions, /npm (?:view|show) skills@latest/);
  assert.doesNotMatch(instructions, /SKILLS_VERSION|BOOTSTRAP_COMMIT|OPSAIL_COMMIT/);
  assert.doesNotMatch(
    instructions,
    /\b(?:opsail|skills)@v?\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?\b/,
  );

  assert.match(commands, /npm install --global "opsail@\$\{OPSAIL_VERSION\}"/);
  assert.match(commands, /OPSAIL_VERSION="\$OPSAIL_VERSION" sh "\$OPSAIL_INSTALLER"/);
  assert.match(commands, /\$OpsailInstaller -Version \$OpsailVersion/);
  assert.match(commands, /npx --yes skills@latest add/);
  assert.match(commands, /github\.com\/lencx\/opsail\/tree\/main\/skills\/opsail/);
  assert.match(
    commands,
    /raw\.githubusercontent\.com\/lencx\/opsail\/refs\/heads\/main\/skills\/opsail\/SKILL\.md/,
  );
  assert.match(commands, /openclaw skills install/);
  assert.match(commands, /openclaw skills install[^\n]*\s--force(?:\s|$)/);
  assert.doesNotMatch(commands, /--force-install/);
  assert.match(
    instructions,
    /\$\{CLAUDE_CONFIG_DIR:-\$HOME\/\.claude\}\/skills/,
  );
  assert.match(instructions, /\$HOME\/\.agents\/skills/);
  assert.match(instructions, /\$HOME\\\.agents/);
  assert.doesNotMatch(instructions, /CODEX_HOME/);
  assert.doesNotMatch(instructions, /\$HOME\/\.codex\/skills/);
  assert.match(instructions, /Plan the CLI and runtime Skill independently/);
  assert.match(instructions, /Ask for approval only for mismatched components/);
  assert.match(instructions, /matching components are verify-only/);
  assert.match(instructions, /execute the same inspected installer/);

  assert.match(shellInstaller, /version="\$\{OPSAIL_VERSION:-latest\}"/);
  assert.match(shellInstaller, /releases\/latest\/download/);
  assert.match(shellInstaller, /releases\/download\/\$tag/);
  assert.match(shellInstaller, /SHA256SUMS/);
  assert.match(
    shellInstaller,
    /expected_hash[\s\S]*actual_hash[\s\S]*"\$actual_hash" = "\$expected_hash"/,
  );
  assert.match(shellInstaller, /installed opsail version mismatch/);
  assert.doesNotMatch(shellInstaller, /\bcurl\b[^\n|]*\|\s*(?:ba)?sh\b/i);

  assert.match(
    powerShellInstaller,
    /\[string\]\s+\$Version\s*=\s*\$env:OPSAIL_VERSION/,
  );
  assert.match(powerShellInstaller, /releases\/latest\/download/);
  assert.match(powerShellInstaller, /SHA256SUMS/);
  assert.match(powerShellInstaller, /Get-FileHash\s+-Algorithm\s+SHA256/);
  assert.match(powerShellInstaller, /installed opsail version mismatch/);
  assert.doesNotMatch(powerShellInstaller, /\birm\b[^\n|]*\|\s*iex\b/i);
});
