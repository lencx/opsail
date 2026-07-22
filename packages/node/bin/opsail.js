#!/usr/bin/env node

import { spawn } from "node:child_process";

import { opsailPath } from "../src/binary.js";

let binaryPath;
try {
  binaryPath = opsailPath();
} catch (error) {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
}

if (binaryPath !== undefined) {
  let child;
  try {
    child = spawn(binaryPath, process.argv.slice(2), {
      shell: false,
      stdio: "inherit",
      windowsHide: false,
    });
  } catch (error) {
    process.stderr.write(`Failed to start Opsail: ${error.message}\n`);
    process.exitCode = 1;
  }

  if (child !== undefined) {
    let settled = false;
    const forwardedSignals = ["SIGINT", "SIGTERM"];
    const signalHandlers = new Map();

    const cleanup = () => {
      for (const [signal, handler] of signalHandlers) {
        process.removeListener(signal, handler);
      }
    };

    for (const signal of forwardedSignals) {
      const handler = () => {
        if (child.exitCode === null && child.signalCode === null) {
          child.kill(signal);
        }
      };
      signalHandlers.set(signal, handler);
      process.on(signal, handler);
    }

    child.once("error", (error) => {
      if (settled) return;
      settled = true;
      cleanup();
      process.stderr.write(`Failed to start Opsail: ${error.message}\n`);
      process.exitCode = 1;
    });

    child.once("exit", (code, signal) => {
      if (settled) return;
      settled = true;
      cleanup();

      if (signal !== null) {
        try {
          process.kill(process.pid, signal);
        } catch {
          process.exitCode = 1;
        }
        return;
      }
      process.exitCode = code ?? 1;
    });
  }
}
