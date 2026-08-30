#!/usr/bin/env bun
// Scope this fixture as a module: `tsconfig.scripts.json` compiles every
// fixture into one global scope, so a script-scoped file collides with the
// identically-named helpers in `openai-compatible.ts` and cannot use
// top-level await.
export {};

/**
 * Deterministic MCP echo server used by the native MCP host tests.
 *
 * One implementation is exposed over the three transports the host supports:
 *   --transport=stdio  newline-delimited JSON-RPC on stdin/stdout
 *   --transport=http   MCP Streamable HTTP on POST/GET/DELETE /mcp
 *
 * The tool list carries every behaviour annotation so the host can be checked
 * for annotation preservation, and the `environment` tool reports the process
 * environment and working directory so the host's argv-only spawn, environment
 * allowlist, and pinned working directory are all observable from a test.
 */

type JsonRpcId = string | number | null;
type JsonRpcRequest = {
  jsonrpc: "2.0";
  id?: JsonRpcId;
  method: string;
  params?: Record<string, unknown>;
};
type JsonRpcResponse = {
  jsonrpc: "2.0";
  id: JsonRpcId;
  result?: unknown;
  error?: { code: number; message: string };
};

function argument(name: string, fallback?: string): string {
  const prefix = `--${name}=`;
  const value = process.argv.find((item) => item.startsWith(prefix))?.slice(prefix.length);
  if (value === undefined) {
    if (fallback !== undefined) return fallback;
    throw new Error(`missing ${prefix}<value>`);
  }
  return value;
}

const transport = argument("transport", "stdio");
const requestedPort = Number.parseInt(argument("port", "0"), 10);
const serverName = argument("name", "mcp-echo");

const TOOLS = [
  {
    name: "echo",
    title: "Echo",
    description: "Returns its arguments unchanged.",
    inputSchema: {
      type: "object",
      properties: { message: { type: "string" } },
      required: ["message"],
    },
    annotations: {
      title: "Echo",
      readOnlyHint: true,
      destructiveHint: false,
      idempotentHint: true,
      openWorldHint: false,
    },
  },
  {
    name: "purge",
    description: "Annotated as an open-world destructive write.",
    inputSchema: { type: "object", properties: { target: { type: "string" } } },
    annotations: {
      readOnlyHint: false,
      destructiveHint: true,
      idempotentHint: false,
      openWorldHint: true,
    },
  },
  {
    name: "environment",
    description: "Reports the environment and working directory of this process.",
    inputSchema: { type: "object" },
    annotations: { readOnlyHint: true },
  },
  {
    name: "unannotated",
    description: "Carries no annotations at all.",
    inputSchema: { type: "object" },
  },
];

function result(id: JsonRpcId, value: unknown): JsonRpcResponse {
  return { jsonrpc: "2.0", id, result: value };
}

function failure(id: JsonRpcId, code: number, message: string): JsonRpcResponse {
  return { jsonrpc: "2.0", id, error: { code, message } };
}

function callTool(id: JsonRpcId, params: Record<string, unknown>): JsonRpcResponse {
  const name = params.name;
  const args = (params.arguments ?? {}) as Record<string, unknown>;
  if (typeof name !== "string" || !TOOLS.some((tool) => tool.name === name)) {
    return failure(id, -32602, `unknown tool: ${String(name)}`);
  }
  if (name === "environment") {
    return result(id, {
      content: [{ type: "text", text: "environment" }],
      structuredContent: {
        names: Object.keys(process.env).sort(),
        cwd: process.cwd(),
        argv: process.argv.slice(2),
      },
      isError: false,
    });
  }
  if (name === "purge") {
    return result(id, { content: [{ type: "text", text: "refused" }], isError: true });
  }
  return result(id, {
    content: [{ type: "text", text: JSON.stringify(args) }],
    structuredContent: { echoed: args, server: serverName },
    isError: false,
  });
}

/** Applies one JSON-RPC request; notifications return null. */
function dispatch(request: JsonRpcRequest): JsonRpcResponse | null {
  const id = request.id ?? null;
  switch (request.method) {
    case "initialize":
      return result(id, {
        protocolVersion: (request.params?.protocolVersion as string) ?? "2025-03-26",
        capabilities: { tools: { listChanged: false }, logging: {} },
        serverInfo: { name: serverName, version: "0.1.0" },
      });
    case "ping":
      return result(id, {});
    case "tools/list":
      return result(id, { tools: TOOLS });
    case "tools/call":
      return callTool(id, request.params ?? {});
    default:
      if (request.id === undefined) return null;
      return failure(id, -32601, `unsupported method: ${request.method}`);
  }
}

async function runStdio(): Promise<void> {
  const decoder = new TextDecoder();
  let buffered = "";
  for await (const chunk of Bun.stdin.stream()) {
    buffered += decoder.decode(chunk as Uint8Array, { stream: true });
    let newline = buffered.indexOf("\n");
    while (newline >= 0) {
      const line = buffered.slice(0, newline).trim();
      buffered = buffered.slice(newline + 1);
      newline = buffered.indexOf("\n");
      if (!line) continue;
      const response = dispatch(JSON.parse(line) as JsonRpcRequest);
      if (response) Bun.stdout.write(`${JSON.stringify(response)}\n`);
    }
  }
}

function jsonResponse(body: unknown, sessionId?: string): Response {
  const headers: Record<string, string> = { "content-type": "application/json" };
  if (sessionId) headers["mcp-session-id"] = sessionId;
  return new Response(JSON.stringify(body), { headers });
}

async function handleStreamableHttp(request: Request): Promise<Response> {
  if (request.method === "DELETE") return new Response(null, { status: 204 });
  if (request.method === "GET") return new Response("no server stream", { status: 405 });
  if (request.method !== "POST") return new Response("method not allowed", { status: 405 });
  const payload = (await request.json()) as JsonRpcRequest | JsonRpcRequest[];
  const batch = Array.isArray(payload) ? payload : [payload];
  const responses = batch.map(dispatch).filter((item): item is JsonRpcResponse => item !== null);
  if (responses.length === 0) return new Response(null, { status: 202 });
  const isInitialize = batch.some((item) => item.method === "initialize");
  const body = Array.isArray(payload) ? responses : responses[0];
  return jsonResponse(body, isInitialize ? `fixture-${Date.now()}` : undefined);
}

function serve(handler: (request: Request) => Promise<Response>): void {
  const server = Bun.serve({ port: requestedPort, hostname: "127.0.0.1", fetch: handler });
  // The Rust test reads this line to learn the bound port.
  console.log(`listening ${server.port}`);
}

if (transport === "stdio") {
  await runStdio();
} else if (transport === "http") {
  serve(handleStreamableHttp);
} else {
  throw new Error(`unsupported transport: ${transport}`);
}
