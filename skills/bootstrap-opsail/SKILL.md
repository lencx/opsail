---
name: bootstrap-opsail
description: Install or update the Opsail CLI and runtime Skill for the current AgentSkills-compatible host, including OpenClaw, Hermes Agent, Claude Code, and Codex. Use when Opsail is missing, mismatched, or not registered.
license: Apache-2.0
compatibility: Requires HTTPS, terminal access, and permission to modify the CLI installation and the current host's global skill directory. Node.js is optional.
metadata: {"author":"Opsail contributors","version":"0.2.0","homepage":"https://github.com/lencx/opsail","opsail-role":"transient-bootstrap"}
---

# Bootstrap Opsail

This is a transient install/update procedure. Do not register it as a global Skill; persist only the `opsail` runtime Skill.

## Plan

Before making persistent changes:

1. Detect the current host. OpenClaw, Hermes Agent, Claude Code, and Codex have dedicated commands below; any other AgentSkills-compatible host follows the generic path. For Hermes, also identify the active profile. Ask only if detection is inconclusive.
2. Resolve `https://api.github.com/repos/lencx/opsail/releases/latest`. If the API fails or is rate limited, resolve the tag from the redirect target of `https://github.com/lencx/opsail/releases/latest`. Require a stable `vMAJOR.MINOR.PATCH` tag and record it without `v` as `OPSAIL_VERSION`.
3. Inspect `opsail --version`, all matching executables (`type -a opsail` or `Get-Command opsail -All`), and the installed runtime Skill. Compare that Skill with `https://raw.githubusercontent.com/lencx/opsail/refs/heads/main/skills/opsail/SKILL.md`.
4. Select adapters:
   - CLI: prefer npm only on Node.js 20+ when `npm view opsail@latest version` equals `OPSAIL_VERSION`. Derive `PLATFORM_PACKAGE` as `@opsail/<process.platform>-<process.arch>`; require `npm view "opsail@${OPSAIL_VERSION}" optionalDependencies --json` to list it at the exact version and `npm view "${PLATFORM_PACKAGE}@${OPSAIL_VERSION}" version` to resolve. Otherwise use the platform installer: `https://raw.githubusercontent.com/lencx/opsail/refs/heads/main/skills/bootstrap-opsail/scripts/install.sh` on macOS and Linux, or `https://raw.githubusercontent.com/lencx/opsail/refs/heads/main/skills/bootstrap-opsail/scripts/install.ps1` on Windows.
   - Runtime Skill: use `npx --yes skills@latest` only with Node.js 18+ and when it names the current host as a supported agent whose global destination matches the host's documented Skill directory. For Hermes, require `HERMES_HOME` to resolve to the intended profile. Install Codex directly into `$HOME/.agents/skills`. Otherwise use the host's native skill-install command. Without one, stage the Skill directory for a copy into the host's documented global skill directory.
5. Plan the CLI and runtime Skill independently. Show all network sources, destinations, the executable that will win on PATH, and every install, update, downgrade, replacement, or PATH change. Ask for approval only for mismatched components; matching components are verify-only.

Stage temporary preflight downloads only for inspection. After approval, execute the same inspected installer, then clean up.

## Apply

If the CLI mismatches, use the selected adapter.

With npm:

```sh
npm install --global "opsail@${OPSAIL_VERSION}"
```

On macOS or Linux, run the inspected installer:

```sh
OPSAIL_VERSION="$OPSAIL_VERSION" sh "$OPSAIL_INSTALLER"
```

Set `OPSAIL_INSTALLER` to the inspected temporary file. Pass `OPSAIL_INSTALL_DIR` only for an approved absolute path.

On Windows, run:

```powershell
& $OpsailInstaller -Version $OpsailVersion
```

Set `$OpsailInstaller` to the inspected temporary file. Use `-InstallDir` only for an approved path and `-UpdatePath` only for an approved persistent PATH change.

If the runtime Skill mismatches, run only the command for the current host. With Node.js 18+, set `OPSAIL_HOST_AGENT` to `openclaw`, `hermes-agent`, `claude-code`, or to another agent name whose `skills@latest` global destination matches the intended host or profile Skill directory:

```sh
npx --yes skills@latest add https://github.com/lencx/opsail/tree/main/skills/opsail --global --agent "$OPSAIL_HOST_AGENT" --copy --yes
```

If the intended Hermes profile destination cannot be verified for `skills@latest`, or without Node.js 18+, use the native command (prefix it with `hermes -p NAME` when targeting a named profile):

```sh
hermes skills install https://raw.githubusercontent.com/lencx/opsail/refs/heads/main/skills/opsail/SKILL.md
```

Without Node.js 18+, OpenClaw requires a local directory. After the planned replacement is approved, download and inspect the same runtime Skill into a temporary directory as `SKILL.md`, then run:

```sh
openclaw skills install "$TEMPORARY_OPSAIL_SKILL_DIRECTORY" --as opsail --global --force
```

Codex uses `$HOME/.agents/skills` regardless of Node.js availability. Without Node.js 18+, Claude Code uses `${CLAUDE_CONFIG_DIR:-$HOME/.claude}/skills`. Stage and inspect the same `SKILL.md` in a temporary directory, set `OPSAIL_HOST_SKILLS_DIR` to the applicable directory, then run:

```sh
mkdir -p "$OPSAIL_HOST_SKILLS_DIR/opsail"
cp "$TEMPORARY_OPSAIL_SKILL_DIRECTORY/SKILL.md" "$OPSAIL_HOST_SKILLS_DIR/opsail/SKILL.md"
```

On Windows, use `$HOME\.agents\skills` for Codex. For Claude Code, append `skills` to a non-empty `$env:CLAUDE_CONFIG_DIR`; otherwise use `$HOME\.claude\skills`. Assign the result to `$OpsailHostSkillsDir`, then run:

```powershell
New-Item -ItemType Directory -Force (Join-Path $OpsailHostSkillsDir "opsail") | Out-Null
Copy-Item (Join-Path $TemporaryOpsailSkillDirectory "SKILL.md") -Destination (Join-Path $OpsailHostSkillsDir "opsail") -Force
```

For any other host, prefer its native skill-install command with the same source. Without one, run the same copy with `OPSAIL_HOST_SKILLS_DIR` set to the host's documented global skill directory.

## Verify

- `opsail --version` reports exactly `opsail ${OPSAIL_VERSION}`.
- `opsail read --help` succeeds.
- The current host discovers the `opsail` Skill and its content matches `main`.

## Constraints

- Configure only the current host unless the user names additional hosts.
- Do not use `sudo`, change npm configuration, or edit shell profiles.
- Stop on failed resolution, download, checksum, installation, or verification. Do not bypass a host security decision or silently switch sources.
- A host-global CLI is not visible inside a sandboxed host, such as an OpenClaw sandbox. Configure the same Opsail version inside the sandbox only with separate approval.
