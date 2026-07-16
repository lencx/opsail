export type ReadSource =
  | {
      kind: "url";
      url: string;
      userAgent?: string;
      acceptLanguage?: string;
    }
  | {
      kind: "html";
      html: string;
      /** URL used to resolve relative links found in the supplied HTML. */
      baseUrl?: string;
      /** Final browser navigation URL recorded as capture provenance. */
      finalUrl?: string;
    }
  | {
      kind: "file";
      path: string;
      baseUrl?: string;
    }
  | {
      kind: "chrome";
      /** HTTP(S) page to navigate to in an Opsail-owned Chrome process. */
      url: string;
      /** Explicit Chrome or Chromium executable path. */
      chromePath?: string;
      /** Browser lifecycle milestone to await after navigation. Defaults to load. */
      waitUntil?: "none" | "dom-content-loaded" | "load" | "network-idle";
      /** User-Agent applied before navigation. */
      userAgent?: string;
      /** Accept-Language value applied before navigation. */
      acceptLanguage?: string;
    }
  | {
      kind: "cdp";
      endpoint: string;
      /** HTTP(S) page to navigate to before capture. */
      url?: string;
      /** Existing Chrome page target to capture or navigate. */
      targetId?: string;
      /** Treat endpoint as a page-scoped WebSocket; incompatible with targetId. */
      directPage?: boolean;
      /** Browser lifecycle milestone to await after navigation. Defaults to load. */
      waitUntil?: "none" | "dom-content-loaded" | "load" | "network-idle";
      /** User-Agent applied before navigation. */
      userAgent?: string;
      /** Accept-Language value applied before navigation. */
      acceptLanguage?: string;
    };

export interface ReadRequest {
  source: ReadSource;
  options?: {
    /** Native acquisition deadline; extraction and bounded cleanup may run afterward. */
    timeoutMs?: number;
    maxBytes?: number;
  };
}

export interface ReadResult {
  schemaVersion: 1;
  content: string;
  contentHtml: string;
  metadata: {
    title: string;
    author?: string;
    description?: string;
    site?: string;
    published?: string;
    modified?: string;
    image?: string;
    favicon?: string;
    language?: string;
    direction?: string;
    canonicalUrl?: string;
    domain?: string;
  };
  source: {
    kind: "url" | "file" | "stdin" | "html" | "chrome" | "cdp" | "memory";
    requested: string;
    resolvedUrl?: string;
    contentType?: string;
    charset: string;
    bytes: number;
  };
  extraction: {
    method: "readability" | "expanded" | "semantic";
    durationMs: number;
  };
  quality: {
    grade: "good" | "fair" | "thin";
    contentCharacters: number;
    wordCount: number;
    extractionRatio: number;
    probablyReadable: boolean;
  };
  warnings: string[];
}

export interface CallOptions {
  signal?: AbortSignal;
}

export interface OpsailConfig {
  binaryPath?: string;
  /** Explicit whole-process deadline; otherwise includes a cleanup margin after timeoutMs. */
  hardTimeoutMs?: number;
  maxOutputBytes?: number;
}

export interface OpsailClient {
  read(request: ReadRequest, options?: CallOptions): Promise<ReadResult>;
}

export class OpsailError extends Error {
  readonly code: string;
  readonly stage: "input" | "acquire" | "extract" | "protocol" | "process";
  readonly retryable: boolean;
  readonly recovery?: string;
  readonly diagnostic?: string;
}

export function read(
  request: ReadRequest,
  options?: CallOptions,
): Promise<ReadResult>;

export function createOpsail(config?: OpsailConfig): OpsailClient;

export function opsailPath(options?: { binaryPath?: string }): string;
