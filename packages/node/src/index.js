import { createOpsail } from "./client.js";

let defaultClient;

export async function read(request, options) {
  defaultClient ??= createOpsail();
  return defaultClient.read(request, options);
}

export { opsailPath } from "./binary.js";
export { createOpsail } from "./client.js";
export { OpsailError } from "./errors.js";
