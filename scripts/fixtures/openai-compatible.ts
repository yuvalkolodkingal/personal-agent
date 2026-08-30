#!/usr/bin/env bun
/** Synthetic OpenAI-compatible endpoint for the isolated runtime compatibility test. */

type ChatMessage = { role?: unknown };
type ChatTool = { name?: unknown; function?: { name?: unknown } };
type ChatRequest = { model?: unknown; messages?: ChatMessage[]; tools?: ChatTool[] };
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
const writePath = argument("write-path");
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

/**
 * Terminal usage chunk in the shape a caller gets back after asking for
 * `stream_options.include_usage`, including the cached-prompt breakdown and
 * the provider-reported cost OpenRouter adds. Selected by model name so the
 * default `deterministic` path stays byte-identical.
 */
function usageStream(): Response {
  return eventStream([
    chunk({ role: "assistant" }),
    chunk({ content: "usage probe complete" }),
    chunk({}, "stop"),
    {
      id: "chatcmpl_fixture",
      object: "chat.completion.chunk",
      created: 1,
      model: "deterministic-usage",
      choices: [],
      usage: {
        prompt_tokens: 41,
        prompt_tokens_details: { cached_tokens: 11 },
        completion_tokens: 7,
        total_tokens: 48,
        cost: 0.00042,
      },
    },
  ]);
}

/**
 * A stream that keeps producing deltas for ~20 s and never finishes, so a
 * client-side abort is observable rather than a race against a buffered body.
 */
function slowStream(): Response {
  const encoder = new TextEncoder();
  const body = new ReadableStream({
    async start(controller): Promise<void> {
      try {
        controller.enqueue(encoder.encode(`data: ${JSON.stringify(chunk({ role: "assistant" }))}\n\n`));
        for (let index = 0; index < 200; index += 1) {
          await Bun.sleep(100);
          const frame = JSON.stringify(chunk({ content: `tick ${index} ` }));
          controller.enqueue(encoder.encode(`data: ${frame}\n\n`));
        }
        controller.close();
      } catch {
        // The client aborted; the stream is already torn down.
      }
    },
  });
  return new Response(body, {
    headers: {
      "cache-control": "no-cache",
      "content-type": "text/event-stream",
    },
  });
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
    const model = typeof payload.model === "string" ? payload.model : "";
    if (model === "deterministic-usage") return usageStream();
    if (model === "deterministic-abort") return slowStream();

    if (hasToolResult && requestMetadata.length >= 3) {
      return eventStream([
        chunk({ role: "assistant" }),
        chunk({ content: "workspace edit and native gateway completed" }),
        chunk({}, "stop"),
      ]);
    }

    if (hasToolResult) {
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
    }

    return eventStream([
      chunk({ role: "assistant" }),
      chunk({
        tool_calls: [
          {
            index: 0,
            id: "call_fixture_write",
            type: "function",
            function: {
              name: "write",
              arguments: JSON.stringify({
                filePath: writePath,
                content: "Personal Agent coding tools are active.\n",
              }),
            },
          },
        ],
      }),
      chunk({}, "tool_calls"),
    ]);
  },
});

process.stdout.write(`${JSON.stringify({ ready: true, port: server.port })}\n`);
