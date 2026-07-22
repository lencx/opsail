import assert from "node:assert/strict";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";

import {
  OpsailError,
  createOpsail,
  opsailPath,
  read,
} from "../src/index.js";

const executable = process.platform === "win32" ? "opsail.exe" : "opsail";
const binaryPath = fileURLToPath(
  new URL(`../../../target/debug/${executable}`, import.meta.url),
);

function sampleHtml() {
  const words = Array.from({ length: 140 }, (_, index) => `word${index}`).join(
    " ",
  );
  return `<!doctype html><html><head><title>Node document</title></head><body><main><article><p>${words}</p><a href="../related">Related coverage</a></article></main></body></html>`;
}

test("read returns the native ReadResult", async () => {
  const opsail = createOpsail({ binaryPath });
  const result = await opsail.read({
    source: {
      kind: "html",
      html: sampleHtml(),
      baseUrl: "https://static.example.test/articles/current/",
      finalUrl: "https://reader.example.test/final/article",
    },
  });

  assert.equal(result.schemaVersion, 1);
  assert.equal(result.metadata.title, "Node document");
  assert.equal(
    result.metadata.canonicalUrl,
    "https://reader.example.test/final/article",
  );
  assert.match(
    result.contentHtml,
    /href="https:\/\/static\.example\.test\/articles\/related"/,
  );
  assert.equal(result.source.kind, "html");
  assert.equal(
    result.source.requested,
    "https://reader.example.test/final/article",
  );
  assert.equal(
    result.source.resolvedUrl,
    "https://reader.example.test/final/article",
  );
});

test("read maps native failures to OpsailError", async () => {
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read({ source: { kind: "html", html: "not html" } }),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "not-html");
      assert.equal(error.stage, "input");
      assert.equal(error.retryable, false);
      return true;
    },
  );
});

test("read forwards Chrome CDP sources through the machine protocol", async () => {
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read({
      source: {
        kind: "cdp",
        endpoint: "ftp://example.test/devtools/browser/id",
        url: "https://example.test/article",
        directPage: true,
        waitUntil: "network-idle",
        userAgent: "opsail-node-contract-test/1.0",
        acceptLanguage: "en-US,en;q=0.9",
      },
    }),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "invalid-cdp-endpoint");
      assert.equal(error.stage, "input");
      return true;
    },
  );
});

test("read enforces the CDP direct-page target constraint", async () => {
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read({
      source: {
        kind: "cdp",
        endpoint: "ws://127.0.0.1:9222/devtools/page/page-id",
        targetId: "page-id",
        directPage: true,
      },
    }),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "invalid-cdp-target");
      assert.equal(error.stage, "input");
      return true;
    },
  );
});

test("read forwards owned Chrome sources through the machine protocol", async () => {
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read({
      source: {
        kind: "chrome",
        url: "https://example.test/article",
        chromePath: "",
        waitUntil: "network-idle",
        userAgent: "opsail-node-contract-test/1.0",
        acceptLanguage: "en-US,en;q=0.9",
      },
    }),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "invalid-chrome-path");
      assert.equal(error.stage, "input");
      return true;
    },
  );
});

test("read reports a missing native binary without parsing human output", async () => {
  const missing = path.join(os.tmpdir(), "missing-opsail-binary");
  const opsail = createOpsail({ binaryPath: missing });

  await assert.rejects(
    opsail.read({ source: { kind: "html", html: sampleHtml() } }),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "binary-not-found");
      assert.equal(error.stage, "process");
      return true;
    },
  );
});

test("read honors a signal that is already aborted", async () => {
  const controller = new AbortController();
  controller.abort();
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read(
      { source: { kind: "html", html: sampleHtml() } },
      { signal: controller.signal },
    ),
    (error) => error instanceof OpsailError && error.code === "aborted",
  );
});

test("read honors an abort raised while serializing the request", async () => {
  const controller = new AbortController();
  const source = { kind: "html" };
  Object.defineProperty(source, "html", {
    enumerable: true,
    get() {
      controller.abort();
      return sampleHtml();
    },
  });
  const opsail = createOpsail({ binaryPath });

  await assert.rejects(
    opsail.read({ source }, { signal: controller.signal }),
    (error) => error instanceof OpsailError && error.code === "aborted",
  );
});

test("read enforces the configured output limit", async () => {
  const opsail = createOpsail({ binaryPath, maxOutputBytes: 100 });

  await assert.rejects(
    opsail.read({ source: { kind: "html", html: sampleHtml() } }),
    (error) =>
      error instanceof OpsailError && error.code === "output-limit-exceeded",
  );
});

test("read aborts an in-flight native request", async () => {
  await withHangingServer(async (url) => {
    const controller = new AbortController();
    const opsail = createOpsail({ binaryPath, hardTimeoutMs: 5_000 });
    const pending = opsail.read(
      { source: { kind: "url", url } },
      { signal: controller.signal },
    );
    setTimeout(() => controller.abort(), 50);

    await assert.rejects(
      pending,
      (error) => error instanceof OpsailError && error.code === "aborted",
    );
  });
});

test("read enforces the process hard timeout", async () => {
  await withHangingServer(async (url) => {
    const opsail = createOpsail({ binaryPath, hardTimeoutMs: 100 });

    await assert.rejects(
      opsail.read({ source: { kind: "url", url } }),
      (error) =>
        error instanceof OpsailError && error.code === "process-timeout",
    );
  });
});

test("opsailPath accepts explicit and environment configuration", () => {
  const previous = process.env.OPSAIL_BINARY_PATH;
  try {
    assert.equal(opsailPath({ binaryPath }), binaryPath);
    process.env.OPSAIL_BINARY_PATH = binaryPath;
    assert.equal(opsailPath(), binaryPath);
  } finally {
    if (previous === undefined) {
      delete process.env.OPSAIL_BINARY_PATH;
    } else {
      process.env.OPSAIL_BINARY_PATH = previous;
    }
  }
});

test("the root read export resolves OPSAIL_BINARY_PATH lazily", async () => {
  const previous = process.env.OPSAIL_BINARY_PATH;
  try {
    process.env.OPSAIL_BINARY_PATH = binaryPath;
    const result = await read({
      source: { kind: "html", html: sampleHtml() },
    });
    assert.equal(result.metadata.title, "Node document");
  } finally {
    if (previous === undefined) {
      delete process.env.OPSAIL_BINARY_PATH;
    } else {
      process.env.OPSAIL_BINARY_PATH = previous;
    }
  }
});

async function withHangingServer(run) {
  const sockets = new Set();
  const server = http.createServer(() => {});
  server.on("connection", (socket) => {
    sockets.add(socket);
    socket.once("close", () => sockets.delete(socket));
  });
  await new Promise((resolve, reject) => {
    server.once("error", reject);
    server.listen(0, "127.0.0.1", resolve);
  });
  const address = server.address();
  const url = `http://127.0.0.1:${address.port}/hang`;

  try {
    await run(url);
  } finally {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
}
