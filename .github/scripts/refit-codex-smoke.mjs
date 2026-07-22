import { spawnSync } from "node:child_process";
import process from "node:process";

const binary = process.env.OPSAIL_BINARY;
if (!binary) {
  throw new Error("OPSAIL_BINARY must point to the native opsail executable");
}

function run(args) {
  const result = spawnSync(binary, args, {
    encoding: "utf8",
    env: {
      ...process.env,
      NO_COLOR: "1",
      RUST_BACKTRACE: "0",
    },
  });

  if (result.error) {
    throw result.error;
  }

  return result;
}

function requireCondition(condition, message, result) {
  if (condition) {
    return;
  }

  const details = result
    ? `\nexit: ${result.status}\nstdout:\n${result.stdout}\nstderr:\n${result.stderr}`
    : "";
  throw new Error(`${message}${details}`);
}

function parseDoctorReport(result) {
  requireCondition(result.status === 0, "doctor did not return a report", result);
  try {
    return JSON.parse(result.stdout);
  } catch (error) {
    throw new Error(
      `doctor stdout is not JSON: ${error.message}\nstdout:\n${result.stdout}`,
    );
  }
}

function check(report, name) {
  return report.checks?.find((entry) => entry.name === name);
}

const help = run(["refit", "codex", "--help"]);
requireCondition(help.status === 0, "refit codex --help failed", help);
for (const command of ["enable", "disable", "status", "doctor", "update"]) {
  requireCondition(
    help.stdout.includes(command),
    `refit codex --help does not list ${command}`,
    help,
  );
}

const doctor = run(["refit", "codex", "doctor"]);

if (process.platform === "win32") {
  // GitHub's hosted Windows image does not contain the Microsoft Store app. The
  // command must still initialize the Windows adapter and report that absence as
  // an application check, rather than falling back to the unsupported platform.
  const report = parseDoctorReport(doctor);
  const platform = check(report, "platform");
  const application = check(report, "application");

  requireCondition(
    report.supported === true,
    "Windows was reported as unsupported",
    doctor,
  );
  requireCondition(
    report.ready === false,
    "doctor was unexpectedly ready without ChatGPT",
    doctor,
  );
  requireCondition(
    platform?.state === "pass",
    "Windows platform check did not pass",
    doctor,
  );
  requireCondition(
    application?.state === "fail",
    "missing Store application was not reported by the application check",
    doctor,
  );
  requireCondition(
    application.message?.startsWith("target-not-found:"),
    "missing Store application did not use target-not-found",
    doctor,
  );
} else if (process.platform === "darwin") {
  const report = parseDoctorReport(doctor);
  requireCondition(
    report.supported === true,
    "macOS was reported as unsupported",
    doctor,
  );
  requireCondition(
    check(report, "platform")?.state === "pass",
    "macOS platform check did not pass",
    doctor,
  );
} else if (process.platform === "linux") {
  if (doctor.status === 0) {
    const report = parseDoctorReport(doctor);
    requireCondition(
      report.supported === false,
      "Linux was unexpectedly reported as supported",
      doctor,
    );
    requireCondition(
      check(report, "platform")?.state === "fail",
      "Linux platform check did not fail",
      doctor,
    );
  } else {
    requireCondition(
      doctor.stderr.includes("[opsail-refit-codex:unsupported]"),
      "Linux doctor did not return the bounded unsupported diagnostic",
      doctor,
    );
  }
} else {
  throw new Error(`unrecognized CI platform: ${process.platform}`);
}

console.log(`refit-codex native smoke passed on ${process.platform}`);
