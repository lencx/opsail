import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";

import { opsailPath } from "./binary.js";
import { OpsailError } from "./errors.js";

const PROTOCOL_VERSION = 1;
const DEFAULT_HARD_TIMEOUT_MS = 30_000;
const HARD_TIMEOUT_GRACE_MS = 10_000;
const DEFAULT_MAX_OUTPUT_BYTES = 32 * 1024 * 1024;
const KILL_GRACE_MS = 5_000;
const MAX_DIAGNOSTIC_BYTES = 4 * 1024;
const MAX_DIAGNOSTIC_INPUT_BYTES = 16 * 1024;
const SOURCE_KINDS = new Set([
  "url",
  "file",
  "stdin",
  "html",
  "chrome",
  "cdp",
  "memory",
]);
const EXTRACTION_METHODS = new Set(["readability", "expanded", "semantic"]);
const QUALITY_GRADES = new Set(["good", "fair", "thin"]);
const MACHINE_FAILURE_STAGES = new Set(["input", "acquire", "extract"]);
const MACHINE_RECOVERIES = new Set(["rendered-html"]);
const OPTIONAL_METADATA_STRINGS = [
  "author",
  "description",
  "site",
  "published",
  "modified",
  "image",
  "favicon",
  "language",
  "direction",
  "canonicalUrl",
  "domain",
];
const OPTIONAL_SOURCE_STRINGS = ["resolvedUrl", "contentType"];

export function createOpsail(config = {}) {
  if (!isRecord(config)) {
    throw new TypeError("createOpsail config must be an object");
  }
  const configuredHardTimeoutMs = positiveInteger(
    config.hardTimeoutMs,
    undefined,
    "hardTimeoutMs",
  );
  const maxOutputBytes = positiveInteger(
    config.maxOutputBytes,
    DEFAULT_MAX_OUTPUT_BYTES,
    "maxOutputBytes",
  );

  return Object.freeze({
    read(request, callOptions = {}) {
      return readWithConfig(
        request,
        callOptions,
        config.binaryPath,
        configuredHardTimeoutMs,
        maxOutputBytes,
      );
    },
  });
}

async function readWithConfig(
  request,
  callOptions,
  configuredBinaryPath,
  configuredHardTimeoutMs,
  maxOutputBytes,
) {
  if (!isRecord(request) || !isRecord(request.source)) {
    throw new OpsailError("read request must contain a source object", {
      code: "invalid-request",
      stage: "input",
    });
  }
  if (!isRecord(callOptions)) {
    throw new TypeError("read call options must be an object");
  }
  const { signal } = callOptions;
  if (signal !== undefined && !isAbortSignal(signal)) {
    throw new TypeError("signal must be an AbortSignal");
  }
  if (signal?.aborted) {
    throw abortedError();
  }

  const binaryPath = opsailPath({ binaryPath: configuredBinaryPath });
  let body;
  let hardTimeoutMs;
  try {
    hardTimeoutMs = resolveHardTimeoutMs(
      configuredHardTimeoutMs,
      request.options?.timeoutMs,
    );
    body = JSON.stringify({ ...request, protocolVersion: PROTOCOL_VERSION });
  } catch (cause) {
    if (signal?.aborted) {
      throw abortedError();
    }
    throw new OpsailError("read request cannot be serialized as JSON", {
      code: "invalid-request",
      stage: "input",
      cause,
    });
  }
  if (signal?.aborted) {
    throw abortedError();
  }

  return invokeMachine({
    binaryPath,
    body,
    signal,
    hardTimeoutMs,
    maxOutputBytes,
    ownsChrome: request.source.kind === "chrome",
  });
}

export function resolveHardTimeoutMs(
  configuredHardTimeoutMs,
  requestTimeoutMs,
) {
  if (configuredHardTimeoutMs !== undefined) return configuredHardTimeoutMs;
  if (!Number.isSafeInteger(requestTimeoutMs) || requestTimeoutMs <= 0) {
    return DEFAULT_HARD_TIMEOUT_MS;
  }
  return Math.max(
    DEFAULT_HARD_TIMEOUT_MS,
    requestTimeoutMs + HARD_TIMEOUT_GRACE_MS,
  );
}

function invokeMachine({
  binaryPath,
  body,
  signal,
  hardTimeoutMs,
  maxOutputBytes,
  ownsChrome,
}) {
  return new Promise((resolve, reject) => {
    let chromeTempRoot;
    try {
      if (ownsChrome) {
        chromeTempRoot = mkdtempSync(
          path.join(os.tmpdir(), "opsail-node-chrome-"),
        );
      }
    } catch (cause) {
      reject(spawnError(cause));
      return;
    }

    let child;
    try {
      child = spawn(binaryPath, ["read", "--machine"], {
        detached: process.platform !== "win32",
        env:
          chromeTempRoot === undefined
            ? process.env
            : { ...process.env, OPSAIL_CHROME_TEMP_ROOT: chromeTempRoot },
        shell: false,
        stdio: ["pipe", "pipe", "pipe"],
        windowsHide: true,
      });
    } catch (cause) {
      cleanupChromeTempRoot(chromeTempRoot);
      reject(spawnError(cause));
      return;
    }

    const stdout = [];
    const stderr = [];
    let outputBytes = 0;
    let stderrBytes = 0;
    let settled = false;
    let terminalError;
    let hardTimeout;
    let killTimer;

    const stopTriggers = () => {
      clearTimeout(hardTimeout);
      signal?.removeEventListener("abort", onAbort);
    };

    const cleanup = () => {
      stopTriggers();
      clearTimeout(killTimer);
      cleanupChromeTempRoot(chromeTempRoot);
    };

    const settleResolve = (value) => {
      if (settled) return;
      settled = true;
      cleanup();
      resolve(value);
    };

    const settleReject = (error) => {
      if (settled) return;
      settled = true;
      cleanup();
      reject(error);
    };

    const finalizeTermination = () => {
      if (terminalError === undefined || settled) return;
      settleReject(terminalError());
    };

    const killProcess = (signalName) => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      try {
        child.kill(signalName);
      } catch {
        // The bounded termination timer remains authoritative.
      }
    };

    const forceKillTree = () => {
      if (child.exitCode !== null || child.signalCode !== null) return;
      if (process.platform !== "win32" && child.pid !== undefined) {
        try {
          process.kill(-child.pid, "SIGKILL");
          return;
        } catch {
          // Fall through when the process group no longer exists.
        }
      }
      killProcess("SIGKILL");
    };

    const terminate = () => {
      killProcess("SIGTERM");
      killTimer = setTimeout(() => {
        forceKillTree();
        finalizeTermination();
      }, KILL_GRACE_MS);
    };

    const requestTermination = (createError) => {
      if (settled || terminalError !== undefined) return;
      terminalError = createError;
      stopTriggers();
      terminate();
    };

    hardTimeout = setTimeout(() => {
      requestTermination(
        () =>
          new OpsailError(`Opsail process exceeded ${hardTimeoutMs}ms`, {
            code: "process-timeout",
            stage: "process",
            retryable: true,
            ...diagnosticOption(stderr),
          }),
      );
    }, hardTimeoutMs);
    hardTimeout.unref?.();

    const onAbort = () => {
      requestTermination(abortedError);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
    if (signal?.aborted) {
      onAbort();
    }

    const appendDiagnostic = (chunk) => {
      const remaining = MAX_DIAGNOSTIC_INPUT_BYTES - stderrBytes;
      if (remaining <= 0) return;
      const slice = chunk.subarray(0, remaining);
      stderr.push(slice);
      stderrBytes += slice.length;
    };

    const collectStdout = (chunk) => {
      if (settled || terminalError !== undefined) return;
      outputBytes += chunk.length;
      if (outputBytes > maxOutputBytes) {
        requestTermination(
          () =>
            new OpsailError(
              `Opsail process output exceeds the ${maxOutputBytes} byte limit`,
              {
                code: "output-limit-exceeded",
                stage: "process",
                ...diagnosticOption(stderr),
              },
            ),
        );
        return;
      }
      stdout.push(chunk);
    };

    const collectStderr = (chunk) => {
      if (settled) return;
      appendDiagnostic(chunk);
      if (terminalError !== undefined) return;
      outputBytes += chunk.length;
      if (outputBytes > maxOutputBytes) {
        requestTermination(
          () =>
            new OpsailError(
              `Opsail process output exceeds the ${maxOutputBytes} byte limit`,
              {
                code: "output-limit-exceeded",
                stage: "process",
                ...diagnosticOption(stderr),
              },
            ),
        );
      }
    };

    child.stdout.on("data", collectStdout);
    child.stderr.on("data", collectStderr);
    child.stdin.on("error", () => {
      // The process response and exit status remain authoritative.
    });

    child.once("error", (cause) => {
      if (terminalError === undefined) {
        settleReject(spawnError(cause));
      }
    });

    child.once("close", (code, signalCode) => {
      clearTimeout(killTimer);
      if (settled) {
        cleanupChromeTempRoot(chromeTempRoot);
        return;
      }
      if (terminalError !== undefined) {
        finalizeTermination();
        return;
      }

      try {
        const result = parseMachineResponse(
          Buffer.concat(stdout),
          code,
          signalCode,
          Buffer.concat(stderr),
        );
        settleResolve(result);
      } catch (error) {
        settleReject(error);
      }
    });

    if (terminalError === undefined) {
      child.stdin.end(body);
    } else {
      child.stdin.end();
    }
  });
}

export function parseMachineResponse(stdout, exitCode, signalCode, stderr) {
  const diagnostic = diagnosticOption(stderr);
  const text = stdout.toString("utf8").trim();
  if (text.length === 0) {
    throw new OpsailError(
      signalCode === null
        ? `Opsail process exited with code ${exitCode ?? "unknown"} without a response`
        : `Opsail process exited from signal ${signalCode} without a response`,
      {
        code: "process-failed",
        stage: "process",
        retryable: true,
        ...diagnostic,
      },
    );
  }

  let response;
  try {
    response = JSON.parse(text);
  } catch (cause) {
    throw new OpsailError("Opsail process returned invalid JSON", {
      code: "invalid-response",
      stage: "protocol",
      cause,
      ...diagnostic,
    });
  }

  if (!isRecord(response) || response.protocolVersion !== PROTOCOL_VERSION) {
    throw new OpsailError("Opsail process returned an unsupported protocol response", {
      code: "protocol-mismatch",
      stage: "protocol",
      ...diagnostic,
    });
  }

  if (!isEngineInfo(response.engine)) {
    throw new OpsailError("Opsail process returned an invalid protocol envelope", {
      code: "invalid-response",
      stage: "protocol",
      ...diagnostic,
    });
  }

  if (response.ok === true && isReadResult(response.result)) {
    if (exitCode !== 0 || signalCode !== null) {
      throw new OpsailError("Opsail success response disagrees with its exit code", {
        code: "protocol-mismatch",
        stage: "protocol",
        ...diagnostic,
      });
    }
    return response.result;
  }

  if (response.ok === false && isMachineFailure(response.error)) {
    if (exitCode !== 1 || signalCode !== null) {
      throw new OpsailError("Opsail failure response disagrees with its exit code", {
        code: "protocol-mismatch",
        stage: "protocol",
        ...diagnostic,
      });
    }
    throw new OpsailError(response.error.message, {
      code: response.error.code,
      stage: response.error.stage,
      retryable: response.error.retryable,
      ...(response.error.recovery === undefined
        ? {}
        : { recovery: response.error.recovery }),
    });
  }

  throw new OpsailError("Opsail process returned an invalid protocol envelope", {
    code: "invalid-response",
    stage: "protocol",
    ...diagnostic,
  });
}

function spawnError(cause) {
  const missing = cause && typeof cause === "object" && cause.code === "ENOENT";
  return new OpsailError(
    missing ? "Opsail binary was not found" : "Opsail process could not be started",
    {
      code: missing ? "binary-not-found" : "spawn-failed",
      stage: "process",
      cause,
    },
  );
}

function cleanupChromeTempRoot(directory) {
  if (directory === undefined) return;
  try {
    rmSync(directory, {
      recursive: true,
      force: true,
      maxRetries: 5,
      retryDelay: 100,
    });
  } catch {
    // Native cleanup or the OS process container remains authoritative.
  }
}

function abortedError() {
  return new OpsailError("Opsail read was aborted", {
    code: "aborted",
    stage: "process",
  });
}

function positiveInteger(value, fallback, name) {
  if (value === undefined) return fallback;
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new TypeError(`${name} must be a positive safe integer`);
  }
  return value;
}

function isAbortSignal(value) {
  return (
    value !== null &&
    typeof value === "object" &&
    typeof value.aborted === "boolean" &&
    typeof value.addEventListener === "function" &&
    typeof value.removeEventListener === "function"
  );
}

function isMachineFailure(value) {
  return (
    isRecord(value) &&
    typeof value.code === "string" &&
    MACHINE_FAILURE_STAGES.has(value.stage) &&
    typeof value.message === "string" &&
    typeof value.retryable === "boolean" &&
    (value.recovery === undefined || MACHINE_RECOVERIES.has(value.recovery))
  );
}

function isEngineInfo(value) {
  return (
    isRecord(value) &&
    value.name === "opsail" &&
    typeof value.version === "string" &&
    value.version.length > 0
  );
}

function isReadResult(value) {
  return (
    isRecord(value) &&
    value.schemaVersion === 1 &&
    typeof value.content === "string" &&
    typeof value.contentHtml === "string" &&
    isRecord(value.metadata) &&
    typeof value.metadata.title === "string" &&
    OPTIONAL_METADATA_STRINGS.every((field) =>
      isOptionalString(value.metadata[field]),
    ) &&
    isRecord(value.source) &&
    SOURCE_KINDS.has(value.source.kind) &&
    typeof value.source.requested === "string" &&
    OPTIONAL_SOURCE_STRINGS.every((field) =>
      isOptionalString(value.source[field]),
    ) &&
    typeof value.source.charset === "string" &&
    isNonNegativeSafeInteger(value.source.bytes) &&
    isRecord(value.extraction) &&
    EXTRACTION_METHODS.has(value.extraction.method) &&
    isNonNegativeSafeInteger(value.extraction.durationMs) &&
    isRecord(value.quality) &&
    QUALITY_GRADES.has(value.quality.grade) &&
    isNonNegativeSafeInteger(value.quality.contentCharacters) &&
    isNonNegativeSafeInteger(value.quality.wordCount) &&
    Number.isFinite(value.quality.extractionRatio) &&
    value.quality.extractionRatio >= 0 &&
    value.quality.extractionRatio <= 1 &&
    typeof value.quality.probablyReadable === "boolean" &&
    Array.isArray(value.warnings) &&
    value.warnings.every((warning) => typeof warning === "string")
  );
}

function isOptionalString(value) {
  return value === undefined || typeof value === "string";
}

function isNonNegativeSafeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function diagnosticOption(stderr) {
  const diagnostic = sanitizeDiagnostic(stderr);
  return diagnostic === undefined ? {} : { diagnostic };
}

function sanitizeDiagnostic(stderr) {
  const input = diagnosticInput(stderr);
  if (input.length === 0) return undefined;

  const sanitized = input
    .toString("utf8")
    .replace(
      /(?:\u001B\]|\u009D)[^\u0007\u009C]*(?:\u0007|\u009C|\u001B\\)/gu,
      "",
    )
    .replace(/(?:\u001B\[|\u009B)[0-?]*[ -/]*[@-~]/gu, "")
    .replace(/\r\n?/gu, "\n")
    .replace(
      /[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu,
      "",
    )
    .trim();
  if (sanitized.length === 0) return undefined;

  const encoded = Buffer.from(sanitized, "utf8");
  if (encoded.length <= MAX_DIAGNOSTIC_BYTES) return sanitized;

  const marker = Buffer.from("…", "utf8");
  let end = MAX_DIAGNOSTIC_BYTES - marker.length;
  while (end > 0 && (encoded[end] & 0xc0) === 0x80) end -= 1;
  return `${encoded.subarray(0, end).toString("utf8")}…`;
}

function diagnosticInput(stderr) {
  if (stderr === undefined || stderr === null) return Buffer.alloc(0);

  const values = Array.isArray(stderr) ? stderr : [stderr];
  const chunks = [];
  let remaining = MAX_DIAGNOSTIC_INPUT_BYTES;
  for (const value of values) {
    if (remaining === 0) break;
    const chunk = Buffer.isBuffer(value) ? value : Buffer.from(value);
    const slice = chunk.subarray(0, remaining);
    chunks.push(slice);
    remaining -= slice.length;
  }
  return Buffer.concat(chunks);
}

function isRecord(value) {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}
