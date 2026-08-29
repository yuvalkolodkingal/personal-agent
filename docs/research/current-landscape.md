# Current landscape and source log

Verified 2026-08-29. `competitors.yaml` is the structured inventory;
`capabilities.yaml` is the deduplicated adoption decision. Historical analysis
comes from the read-only Jarvis surveys and is not treated as current without a
fresh primary source.

## Runtime and coding agents

OpenCode remains the runtime base because its stable server is protected,
OpenAPI/SSE driven, provider-neutral, local-provider capable, and extensible by
agents, skills, plugins, and MCP. Version 1.18.23 is pinned. The embedded V2 SDK
remains beta. Current V1 hook limitations mean a before-hook cannot be trusted
as the sole safety mechanism. The coding compatibility surface therefore uses
the pinned runtime's own granular permissions and canonical workspace scope,
plus independent native preauthorization; non-coding effects remain behind
native gateway tools.

OpenClaw and Leon add isolated local workspaces, channel routing, layered
memory, local-provider choice, native/agent skill separation, and bounded
proactivity. Codex, Claude Code, Gemini CLI, Goose, Cline, OpenHands, Aider,
Cursor, and Windsurf contribute durable/parallel agents, worktree or sandbox
isolation, review, rules/skills/hooks, provider choice, repository maps, IDE
context, and approval UX. These collapse into `CAP-AGENT`, `CAP-PROJECTS`,
`CAP-EXTENSIONS`, `CAP-SAFETY`, and `CAP-WORKSPACE` rather than product-specific
copies.

## Browser agents

Playwright MCP and Chrome DevTools MCP establish structured accessibility/DOM
and CDP-first control. browser-use, agent-browser, Stagehand, and Skyvern add
agent/session isolation, stable element references, action/extraction, and
visual-fallback patterns. None eliminates indirect prompt injection, so
`CAP-BROWSER` couples isolated profiles and opaque generation handles to
data-zone policy, takeover, domain/subresource rules, download quarantine, and
confirmation for commitments.

## Voice and ambient assistants

GLaDOS, Home Assistant Assist, Leon, OpenVoiceOS, Pipecat, LiveKit Agents, TEN,
OpenAI Realtime, Gemini Live, Talon, and commercial assistants contribute
local/offline voice, satellites, wake/skills, streaming pipelines, semantic
turns, barge-in, multilingual and multimodal interaction, dictation, ambient
state, smart-home integrations, and proactive tasks. These collapse into
`CAP-VOICE`, `CAP-CONVERSATION`, `CAP-AUTOMATION`, and official connector packs.

## Productivity and research

Raycast, Alfred, and Talon reinforce the command palette, hotkey, workflow,
dictation, accessibility, extension, clipboard, and quick interaction model.
ChatGPT, Claude/Cowork, Gemini, Alexa+, Microsoft Copilot, Perplexity Computer,
and n8n reinforce cited research, artifacts, scheduled work, connectors,
projects, browser/computer use, takeover, approvals, and durable automation.
Capabilities are adopted without advertising, deceptive sentience, prohibited
scraping, or competitor branding.

## Primary sources

- OpenCode: <https://opencode.ai/docs/>, <https://dev.opencode.ai/docs/server/>,
  <https://opencode.ai/v2/docs/build/sdk>
- OpenClaw: <https://docs.openclaw.ai/>
- Leon: <https://github.com/leon-ai/leon>
- OpenAI Codex: <https://developers.openai.com/codex/>
- Claude Code: <https://docs.anthropic.com/en/docs/claude-code/overview>
- Gemini CLI: <https://github.com/google-gemini/gemini-cli>
- Goose: <https://block.github.io/goose/>
- Cline: <https://docs.cline.bot/>
- OpenHands: <https://docs.all-hands.dev/>
- Aider: <https://aider.chat/docs/>
- Cursor: <https://docs.cursor.com/>
- Windsurf: <https://docs.windsurf.com/>
- Playwright MCP: <https://github.com/microsoft/playwright-mcp>
- Chrome DevTools MCP: <https://github.com/ChromeDevTools/chrome-devtools-mcp>
- browser-use: <https://docs.browser-use.com/>
- agent-browser: <https://github.com/vercel-labs/agent-browser>
- Stagehand: <https://docs.stagehand.dev/>
- Skyvern: <https://docs.skyvern.com/>
- GLaDOS: <https://github.com/dnhkng/GlaDOS>
- Home Assistant voice: <https://developers.home-assistant.io/docs/voice/overview/>
- OpenVoiceOS: <https://openvoiceos.github.io/ovos-technical-manual/>
- Pipecat: <https://docs.pipecat.ai/>
- LiveKit Agents: <https://docs.livekit.io/agents/>
- TEN: <https://github.com/ten-framework/ten-framework>
- OpenAI Realtime: <https://platform.openai.com/docs/guides/realtime>
- Gemini Live: <https://ai.google.dev/gemini-api/docs/live-api>
- Talon: <https://talonvoice.com/docs/>
- Raycast: <https://developers.raycast.com/>
- Alfred: <https://www.alfredapp.com/help/>
- ChatGPT Scheduled Tasks: <https://help.openai.com/en/articles/10291617-scheduled-tasks-in-chatgpt>
- Claude Cowork containment: <https://www.anthropic.com/engineering/how-we-contain-claude>
- Microsoft Copilot Tasks: <https://support.microsoft.com/en-us/microsoft-copilot/using-copilot-tasks>
- Perplexity Computer tasks: <https://www.perplexity.ai/help-center/en/articles/11521526-perplexity-tasks>
- Alexa+: <https://www.aboutamazon.com/what-we-do/devices-services/alexa-plus>
- n8n AI workflows: <https://docs.n8n.io/advanced-ai/>
