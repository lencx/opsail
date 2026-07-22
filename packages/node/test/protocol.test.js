import assert from "node:assert/strict";
import test from "node:test";

import { parseMachineResponse } from "../src/client.js";
import { OpsailError } from "../src/errors.js";

function encode(value) {
  return Buffer.from(JSON.stringify(value));
}

function machineEnvelope(value) {
  return {
    protocolVersion: 1,
    engine: { name: "opsail", version: "0.1.0" },
    ...value,
  };
}

function validResult() {
  return {
    schemaVersion: 1,
    content: "Readable text.",
    contentHtml: "<p>Readable text.</p>",
    metadata: { title: "Example" },
    source: {
      kind: "memory",
      requested: "<memory>",
      charset: "utf-8",
      bytes: 21,
    },
    extraction: { method: "semantic", durationMs: 1 },
    quality: {
      grade: "thin",
      contentCharacters: 14,
      wordCount: 2,
      extractionRatio: 0.5,
      probablyReadable: true,
    },
    warnings: [],
  };
}

function successResponse(result = validResult()) {
  return encode(machineEnvelope({ ok: true, result }));
}

function assertInvalidResult(mutate) {
  const result = validResult();
  mutate(result);

  assert.throws(
    () => parseMachineResponse(successResponse(result), 0, null),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
}

test("machine responses require a versioned ReadResult", () => {
  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: true, result: {} })),
        0,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("machine responses require the Opsail engine identity", () => {
  for (const engine of [
    undefined,
    null,
    { name: "other", version: "0.1.0" },
    { name: "opsail", version: "" },
    { name: "opsail", version: 1 },
  ]) {
    const response = machineEnvelope({ ok: true, result: validResult() });
    response.engine = engine;
    assert.throws(
      () => parseMachineResponse(encode(response), 0, null),
      (error) =>
        error instanceof OpsailError && error.code === "invalid-response",
    );
  }
});

test("machine ReadResult validation covers required strings and objects", () => {
  for (const mutate of [
    (result) => {
      result.content = null;
    },
    (result) => {
      result.contentHtml = 1;
    },
    (result) => {
      result.metadata = [];
    },
    (result) => {
      result.metadata.title = false;
    },
    (result) => {
      result.source = null;
    },
    (result) => {
      result.source.requested = 1;
    },
    (result) => {
      result.source.charset = undefined;
    },
    (result) => {
      result.extraction = [];
    },
    (result) => {
      result.quality = null;
    },
    (result) => {
      result.warnings = ["useful", 1];
    },
  ]) {
    assertInvalidResult(mutate);
  }
});

test("machine ReadResult validation enforces documented enums", () => {
  for (const mutate of [
    (result) => {
      result.source.kind = "browser";
    },
    (result) => {
      result.extraction.method = "custom";
    },
    (result) => {
      result.quality.grade = "excellent";
    },
  ]) {
    assertInvalidResult(mutate);
  }
});

test("machine ReadResult validation accepts captured browser provenance", () => {
  for (const kind of ["html", "chrome", "cdp"]) {
    const result = validResult();
    result.source.kind = kind;
    assert.deepEqual(parseMachineResponse(successResponse(result), 0, null), result);
  }
});

test("machine ReadResult validation enforces non-negative safe counts", () => {
  const setters = [
    (result, value) => {
      result.source.bytes = value;
    },
    (result, value) => {
      result.extraction.durationMs = value;
    },
    (result, value) => {
      result.quality.contentCharacters = value;
    },
    (result, value) => {
      result.quality.wordCount = value;
    },
  ];

  for (const set of setters) {
    for (const value of [-1, 1.5, Number.MAX_SAFE_INTEGER + 1]) {
      assertInvalidResult((result) => set(result, value));
    }
  }
});

test("machine ReadResult validation requires a bounded extraction ratio and boolean readability", () => {
  for (const value of [null, "0.5", false, -0.01, 1.01]) {
    assertInvalidResult((result) => {
      result.quality.extractionRatio = value;
    });
  }
  assertInvalidResult((result) => {
    result.quality.probablyReadable = 1;
  });

  const envelope = JSON.stringify(
    machineEnvelope({
      ok: true,
      result: validResult(),
    }),
  ).replace('"extractionRatio":0.5', '"extractionRatio":1e400');
  assert.throws(
    () => parseMachineResponse(Buffer.from(envelope), 0, null),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("machine ReadResult validation checks optional metadata and source strings", () => {
  const metadataFields = [
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
  for (const field of metadataFields) {
    assertInvalidResult((result) => {
      result.metadata[field] = 1;
    });
  }
  for (const field of ["resolvedUrl", "contentType"]) {
    assertInvalidResult((result) => {
      result.source[field] = false;
    });
  }

  const result = validResult();
  for (const field of metadataFields) result.metadata[field] = field;
  result.source.resolvedUrl = "https://example.test/final";
  result.source.contentType = "text/html";
  assert.deepEqual(parseMachineResponse(successResponse(result), 0, null), result);
});

test("machine success responses require exit code zero without a signal", () => {
  const result = validResult();
  assert.deepEqual(
    parseMachineResponse(
      encode(machineEnvelope({ ok: true, result })),
      0,
      null,
    ),
    result,
  );

  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: true, result })),
        null,
        "SIGTERM",
      ),
    (error) =>
      error instanceof OpsailError && error.code === "protocol-mismatch",
  );
});

test("machine failure responses require the reserved exit code", () => {
  const response = encode(machineEnvelope({
    ok: false,
    error: {
      code: "not-html",
      stage: "input",
      message: "source does not appear to be HTML",
      retryable: false,
    },
  }));

  assert.throws(
    () => parseMachineResponse(response, 1, null),
    (error) => error instanceof OpsailError && error.code === "not-html",
  );
  for (const [exitCode, signalCode] of [
    [2, null],
    [null, "SIGTERM"],
  ]) {
    assert.throws(
      () => parseMachineResponse(response, exitCode, signalCode),
      (error) =>
        error instanceof OpsailError && error.code === "protocol-mismatch",
    );
  }
});

test("machine failures require a native error stage", () => {
  for (const stage of ["unknown-stage", "protocol", "process"]) {
    const response = encode(
      machineEnvelope({
        ok: false,
        error: {
          code: "not-html",
          stage,
          message: "source does not appear to be HTML",
          retryable: false,
        },
      }),
    );

    assert.throws(
      () => parseMachineResponse(response, 1, null),
      (error) =>
        error instanceof OpsailError && error.code === "invalid-response",
    );
  }
});

test("machine failures only accept the native recovery value", () => {
  const failure = {
    code: "verification-required",
    stage: "acquire",
    message: "source requires browser verification",
    retryable: false,
    recovery: "rendered-html",
  };
  assert.throws(
    () =>
      parseMachineResponse(
        encode(machineEnvelope({ ok: false, error: failure })),
        1,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.recovery === "rendered-html",
  );

  assert.throws(
    () =>
      parseMachineResponse(
        encode(
          machineEnvelope({
            ok: false,
            error: { ...failure, recovery: "open-browser" },
          }),
        ),
        1,
        null,
      ),
    (error) =>
      error instanceof OpsailError && error.code === "invalid-response",
  );
});

test("process and protocol errors expose sanitized bounded native diagnostics", () => {
  assert.throws(
    () =>
      parseMachineResponse(
        Buffer.alloc(0),
        2,
        null,
        Buffer.from(
          "\u001b[31mfirst\u001b[0m\u009b32m line\u009b0m\u202e\0\r\nsecond line\u2067",
        ),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "process-failed");
      assert.equal(error.diagnostic, "first line\nsecond line");
      return true;
    },
  );

  assert.throws(
    () =>
      parseMachineResponse(
        Buffer.from("not-json"),
        2,
        null,
        Buffer.from("x".repeat(10_000)),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.code, "invalid-response");
      assert(Buffer.byteLength(error.diagnostic, "utf8") <= 4_096);
      assert.match(error.diagnostic, /…$/u);
      return true;
    },
  );
});

test("valid structured failures only expose whitelisted fields", () => {
  const response = encode(machineEnvelope({
    ok: false,
    error: {
      code: "not-html",
      stage: "input",
      message: "source does not appear to be HTML",
      retryable: false,
      diagnostic: "\u001b[31minjected diagnostic",
      cause: { secret: "injected cause" },
    },
  }));

  assert.throws(
    () =>
      parseMachineResponse(
        response,
        1,
        null,
        Buffer.from("possibly sensitive native detail"),
      ),
    (error) => {
      assert(error instanceof OpsailError);
      assert.equal(error.message, "source does not appear to be HTML");
      assert.equal(error.diagnostic, undefined);
      assert.equal(error.cause, undefined);
      return true;
    },
  );
});
