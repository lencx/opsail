export class OpsailError extends Error {
  constructor(message, options = {}) {
    if (options.cause === undefined) {
      super(message);
    } else {
      super(message, { cause: options.cause });
    }
    this.name = "OpsailError";
    this.code = options.code ?? "unknown";
    this.stage = options.stage ?? "process";
    this.retryable = options.retryable ?? false;
    if (options.recovery !== undefined) {
      this.recovery = options.recovery;
    }
    if (options.diagnostic !== undefined) {
      this.diagnostic = options.diagnostic;
    }
  }
}
