/**
 * Local (CRW_LOCAL) subprocess transport: speaks MCP JSON-RPC to a `crw-mcp`
 * binary over stdio. Mirrors the Python SDK's subprocess mode.
 *
 * v1 finds the binary via the `CRW_BINARY` env var or on `PATH`; auto-download
 * (as the Python SDK does) is a fast-follow.
 */

import { type ChildProcessByStdio, spawn } from "node:child_process";
import type { Readable, Writable } from "node:stream";
import { CrwApiError, CrwBinaryNotFoundError, CrwError } from "./errors.js";

type McpProc = ChildProcessByStdio<Writable, Readable, null>;
import type { Json } from "./types.js";

const BINARY_NAME = process.platform === "win32" ? "crw-mcp.exe" : "crw-mcp";

interface Pending {
  resolve: (v: Json) => void;
  reject: (e: Error) => void;
}

/**
 * Variables that switch `crw-mcp` from embedded to proxy mode. Both are
 * verified against the shipped binary: `CRW_API_URL` is bound by its CLI, and
 * `CRW_CLIENT__API_URL` reaches the same setting through the config layer.
 */
const PROXY_MODE_ENV_VARS = ["CRW_API_URL", "CRW_CLIENT__API_URL"];

/**
 * Environment for the `crw-mcp` subprocess in CRW_LOCAL mode.
 *
 * The child inherits our environment, so either variable above left in the
 * shell silently turned "run the local engine" into "call the cloud": every
 * tool then answered with the REST envelope instead of the flat payload, and
 * the call was billed. CRW_LOCAL means local, so the child does not get them.
 *
 * This cannot close every route: `crw-mcp` also reads `client.api_url` from
 * `~/.config/crw/config.toml`, and it has no flag to force embedded mode. That
 * is why `map()` still handles the envelope shape in `mapPayload`.
 */
export function localChildEnv(): NodeJS.ProcessEnv {
  // Windows environment names are case-insensitive, so the child would read a
  // differently-spelled key that an exact-match filter left behind. POSIX names
  // are case-sensitive, so there only the exact spellings matter.
  const fold = (k: string) => (process.platform === "win32" ? k.toUpperCase() : k);
  const drop = new Set(PROXY_MODE_ENV_VARS.map(fold));
  return Object.fromEntries(Object.entries(process.env).filter(([k]) => !drop.has(fold(k))));
}

/**
 * Environment for a `crw-mcp` subprocess in PROXY mode: it talks to `apiUrl`
 * with the client's own key, whatever the shell says. Only the data tools use
 * it, for a source the API answered with `client_execution_required`.
 */
export function proxyChildEnv(apiUrl: string, apiKey: string | undefined): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = { ...process.env, CRW_API_URL: apiUrl };
  if (apiKey) env.CRW_API_KEY = apiKey;
  else delete env.CRW_API_KEY;
  return env;
}

export class LocalTransport {
  private proc: McpProc | null = null;
  private nextId = 0;
  private pending = new Map<number, Pending>();
  private buffer = "";

  /** `env` defaults to {@link localChildEnv} (embedded mode). */
  constructor(private readonly env?: NodeJS.ProcessEnv) {}

  private resolveBinary(): string {
    const env = process.env.CRW_BINARY;
    if (env) return env;
    // Rely on PATH resolution by spawning the bare name; if it ENOENTs the
    // error handler surfaces a clear install hint.
    return BINARY_NAME;
  }

  private ensureProcess(): McpProc {
    if (this.proc && this.proc.exitCode === null) return this.proc;
    const bin = this.resolveBinary();
    const proc = spawn(bin, [], { stdio: ["pipe", "pipe", "ignore"], env: this.env ?? localChildEnv() });
    proc.on("error", (err: NodeJS.ErrnoException) => {
      const where = bin === BINARY_NAME ? "on PATH" : `at CRW_BINARY=${bin}`;
      const failure =
        err.code === "ENOENT"
          ? new CrwBinaryNotFoundError(
              `crw-mcp binary not found ${where}. Install it (e.g. \`npm i -g crw-mcp\` or ` +
                `\`cargo install crw-mcp\`) or set CRW_BINARY to its path.`,
            )
          : new CrwError(`crw-mcp failed to start: ${err.message}`);
      for (const p of this.pending.values()) p.reject(failure);
      this.pending.clear();
    });
    proc.stdout.setEncoding("utf8");
    proc.stdout.on("data", (chunk: string) => this.onData(chunk));
    proc.on("exit", () => {
      for (const p of this.pending.values()) p.reject(new CrwError("crw-mcp process closed unexpectedly"));
      this.pending.clear();
    });
    this.proc = proc;
    return proc;
  }

  private onData(chunk: string): void {
    this.buffer += chunk;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      let msg: Json;
      try {
        msg = JSON.parse(line) as Json;
      } catch {
        continue;
      }
      const id = msg.id as number | undefined;
      if (id === undefined || !this.pending.has(id)) continue;
      const p = this.pending.get(id)!;
      this.pending.delete(id);
      if (msg.error) {
        const err = msg.error as { message?: string };
        p.reject(new CrwApiError(err.message ?? JSON.stringify(msg.error)));
      } else {
        p.resolve((msg.result as Json) ?? {});
      }
    }
  }

  /** MCP handshake; `crw-mcp` is stateless, so a respawn needs no repeat. */
  initialize(): Promise<Json> {
    return this.jsonrpc("initialize", {
      protocolVersion: "2025-06-18",
      capabilities: {},
      clientInfo: { name: "crw-sdk", version: "1" },
    });
  }

  jsonrpc(method: string, params: Json): Promise<Json> {
    const proc = this.ensureProcess();
    const id = ++this.nextId;
    return new Promise<Json>((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      proc.stdin.write(`${JSON.stringify({ jsonrpc: "2.0", id, method, params })}\n`);
    });
  }

  async toolCall(name: string, args: Json): Promise<Json> {
    const result = await this.jsonrpc("tools/call", { name, arguments: args });
    const content = (result.content as Array<{ text?: string }> | undefined)?.[0];
    if (!content) throw new CrwError(`Empty response from ${name}`);
    if (result.isError) {
      // A tool error has no HTTP status; its bracketed code is the errorCode.
      const text = content.text ?? "Unknown error";
      throw new CrwApiError(text, undefined, /^\[([a-z_]+)\]/.exec(text)?.[1]);
    }
    return JSON.parse(content.text ?? "{}") as Json;
  }

  /**
   * EOF first so the child can finish what it started (a proxy-mode data call
   * sends a usage ping after answering), then kill it if it is still up after
   * `graceMs`. Resolves once it is gone; not awaiting it is fine.
   */
  close(graceMs = 1000): Promise<void> {
    const proc = this.proc;
    this.proc = null;
    if (!proc || proc.exitCode !== null || proc.signalCode !== null) return Promise.resolve();
    return new Promise<void>((resolve) => {
      const timer = setTimeout(() => {
        proc.kill();
        resolve();
      }, graceMs);
      proc.once("exit", () => {
        clearTimeout(timer);
        resolve();
      });
      proc.stdin.end();
    });
  }
}
