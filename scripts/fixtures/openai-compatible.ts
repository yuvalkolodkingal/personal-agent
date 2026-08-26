#!/usr/bin/env bun
/** Synthetic OpenAI-compatible endpoint for the isolated runtime compatibility test. */

type ChatMessage = { role?: unknown };
type ChatTool = { name?: unknown; function?: { name?: unknown } };
type ChatRequest = { messages?: ChatMessage[]; tools?: ChatTool[] };
type RequestMetadata = {
  bodyKeys: string[];
  hasToolResult: boolean;
  toolCount: number;
  toolKeys: string[][];
  toolNames: string[];
};

function argument(name: string): string {
  const prefix = `--${name}=`;
  const value = process.argv.find((item) => item.startsWith(prefix))?.slice(prefix.length);
  if (!value) throw new Error(`missing ${prefix}<value>`);
  return value;
}

const port = Number.parseInt(argument("port"), 10);
const metadataPath = argument("metadata-path");
const requestMetadata: RequestMetadata[] = [];
if (!Number.isSafeInteger(port) || port < 1 || port > 65535) {
  throw new Error("fixture port is invalid");
}

function eventStream(chunks: unknown[]): Response {
  const body = `${chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("")}data: [DONE]\n\n`;
  return new Response(body, {
    headers: {
      "cache-control": "no-cache",
      "content-type": "text/event-stream",
    },
  });
}

function chunk(delta: unknown, finishReason: string | null = null): unknown {
  return {
    id: "chatcmpl_fixture",
    object: "chat.completion.chunk",
    created: 1,
    model: "deterministic",
    choices: [{ index: 0, delta, finish_reason: finishReason }],
  };
}

const server = Bun.serve({
  hostname: "127.0.0.1",
  port,
  async fetch(request): Promise<Response> {
    const url = new URL(request.url);
    if (request.method !== "POST" || url.pathname !== "/v1/chat/completions") {
      return Response.json({ error: { message: "fixture route not found" } }, { status: 404 });
    }

    const payload = (await request.json()) as ChatRequest;
    const hasToolResult = payload.messages?.some((message) => message.role === "tool") ?? false;
    const requestTools = payload.tools ?? [];
    const toolNames = requestTools
      .map((tool) => tool.function?.name ?? tool.name)
      .filter((name): name is string => typeof name === "string");
    requestMetadata.push({
      bodyKeys: Object.keys(payload).sort(),
      hasToolResult,
      toolCount: requestTools.length,
      toolKeys: requestTools.map((tool) => Object.keys(tool).sort()),
      toolNames,
    });
    await Bun.write(metadataPath, `${JSON.stringify(requestMetadata)}\n`);
    if (hasToolResult) {
      return eventStream([
        chunk({ role: "assistant" }),
        chunk({ content: "native gateway completed" }),
        chunk({}, "stop"),
      ]);
    }

    return eventStream([
      chunk({ role: "assistant" }),
      chunk({
        tool_calls: [
          {
            index: 0,
            id: "call_fixture_gateway",
            type: "function",
            function: {
              name: "personal_agent_gateway_status",
              arguments: JSON.stringify({}),
            },
          },
        ],
      }),
      chunk({}, "tool_calls"),
    ]);
  },
});

process.stdout.write(`${JSON.stringify({ ready: true, port: server.port })}\n`);
