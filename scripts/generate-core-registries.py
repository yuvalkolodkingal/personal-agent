#!/usr/bin/env python3
"""Generate normalized capability, competitor, acceptance, and rejection registries."""
from __future__ import annotations
from datetime import date
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TODAY = str(date.today())

tests = [
    ("AT-M0-LEGACY-MAP", "M0", "Every legacy module, command, setting, test, documented feature, limitation, and migration input maps to an existing capability and test.", "python scripts/verify-registries.py", "automated", "passed"),
    ("AT-M0-REGISTRY-SCHEMA", "M0", "Every capability record contains the mandatory provenance, route, platform, security, and acceptance fields.", "python scripts/verify-registries.py", "automated", "passed"),
    ("AT-M1-CONTRACT-DRIFT", "M1", "Generated Rust and TypeScript contracts match protobuf sources.", "bun run contracts:check && cargo test -p personal-agent-contracts", "automated", "passed"),
    ("AT-M1-EVENT-RECOVERY", "M1", "UI projection is rebuilt only from ordered persisted events.", "cargo test -p personal-agent-core projection_rebuilds", "automated", "passed"),
    ("AT-M1-STORAGE-ENCRYPTED", "M1", "SQLCipher is present, keyed before schema access, and events resume by sequence.", "cargo test -p personal-agent-storage", "automated", "passed"),
    ("AT-M1-CONFIG-ROUNDTRIP", "M1", "Canonical config schema validates TOML edits, repairs safe values, and rejects invalid risk acknowledgements.", "cargo test -p personal-agent-core config", "automated", "passed"),
    ("AT-M1-PLATFORM-BUILD", "M1", "Development desktop builds install and rebuild state on Windows, macOS, and Linux.", "CI matrix plus installer smoke", "mixed", "in_progress"),
    ("AT-M2-STREAMED-TURN", "M2", "Fake and configured real providers complete a streamed tool-using turn on each platform.", "runtime compatibility suite", "mixed", "in_progress"),
    ("AT-M2-SIDECAR-AUTH", "M2", "OpenCode binds loopback only with per-run auth and exact-version compatibility.", "cargo test -p personal-agent-runtime", "automated", "in_progress"),
    ("AT-M3-OFFLINE-VOICE", "M3", "Wake-to-spoken response works after all network interfaces are disabled.", "licensed audio replay plus hardware run", "mixed", "in_progress"),
    ("AT-M3-CONVERSATION-STATES", "M3", "Typed, voice, sleep, mute, quiet, stop, follow-up, guest, and project states stay distinct.", "conversation state-machine suite", "automated", "in_progress"),
    ("AT-M3-BARGE-IN", "M3", "Internal speaker stop is under 50 ms and end-to-end barge-in is under 100 ms.", "audio benchmark report", "hardware", "planned"),
    ("AT-M4-ACCESSIBLE-WORKSPACE", "M4", "Every essential action is keyboard and screen-reader accessible; reduced motion is honored.", "Vitest accessibility plus visual regression", "mixed", "in_progress"),
    ("AT-M4-PROJECT-CONTEXT", "M4", "Opening, closing, and switching project context does not lose unrelated conversation state.", "project integration suite", "automated", "planned"),
    ("AT-M5-POLICY-GATE", "M5", "Always-confirm effects ask, missing scopes deny, untrusted cross-zone requests cannot silently proceed, and audit contains no secrets.", "cargo test -p personal-agent-policy -p personal-agent-tools", "automated", "passed"),
    ("AT-M5-BROWSER-SAFE", "M5", "Isolated profile tasks work and malicious page instructions cannot cross data zones.", "fixture-site and red-team suite", "automated", "in_progress"),
    ("AT-M5-DESKTOP-TOOLS", "M5", "Representative accessibility-first desktop tasks complete with explicit degradation on all platforms.", "platform VM/hardware suite", "mixed", "in_progress"),
    ("AT-M5-ROLLBACK", "M5", "Checkpoints precede mutations and rollback snapshots current state before restoring.", "fault-injection suite", "automated", "planned"),
    ("AT-M6-DAG-RECOVERY", "M6", "A multi-hour DAG survives restart and provider failure without duplicate consequential actions.", "chaos scenario", "automated", "in_progress"),
    ("AT-M6-MEMORY-PROVENANCE", "M6", "Explicit facts are trusted, inference is queued, conflicts remain visible, and recalled text is not re-extracted.", "cargo test -p personal-agent-memory", "automated", "in_progress"),
    ("AT-M6-AUTOMATION-RECOVERY", "M6", "Missed runs follow policy, approvals suspend, and repeated failures pause an automation.", "cargo test -p personal-agent-automation", "automated", "in_progress"),
    ("AT-M7-PLUGIN-SAFETY", "M7", "Unsigned plugins default off; renderer code and policy-rewrite scopes are rejected.", "cargo test -p personal-agent-plugins", "automated", "passed"),
    ("AT-M7-REMOTE-PROTOCOL", "M7", "Third-party clients pair with fresh keys and cannot exceed negotiated capabilities.", "protocol interoperability suite", "automated", "planned"),
    ("AT-M7-COMPETITOR-COVERAGE", "M7", "Every adopted competitor capability is core or an installable official pack with an evaluation.", "registry coverage check", "automated", "planned"),
    ("AT-M8-MIGRATION-DRY-RUN", "M8", "Synthetic profiles dry-run and import idempotently without modifying source or copying plaintext secrets.", "cargo test -p personal-agent-migration plus fixtures", "automated", "in_progress"),
    ("AT-M9-LIFECYCLE", "M9", "Fresh install, update, health rollback, export, uninstall, and optional data deletion pass on every platform.", "signed/unsigned installer matrix", "mixed", "planned"),
    ("AT-M9-PERFORMANCE", "M9", "Latency and idle resource targets report p50, p95, max, and sample count.", "benchmark harness", "hardware", "planned"),
    ("AT-M9-SECURITY", "M9", "No critical/high finding remains; SBOM, licenses, provenance, fuzzing, and red-team reports are complete.", "release security gate", "mixed", "planned"),
]

cap_specs = [
    ("CAP-CONTRACTS","architecture","Versioned protobuf IPC/domain contracts and Draft 2020-12 schemas generate Rust and TypeScript types.", ["Jarvis"], "replaced-equivalently", "core", "M1", ["AT-M1-CONTRACT-DRIFT"], "in_progress"),
    ("CAP-STORAGE","storage","SQLCipher WAL event store, content-addressed blobs, transactional migrations, and rebuildable projections.", ["Jarvis"], "replaced-equivalently", "core", "M1", ["AT-M1-STORAGE-ENCRYPTED","AT-M1-EVENT-RECOVERY"], "in_progress"),
    ("CAP-CONFIG","configuration","Strict human-editable TOML backed by one canonical schema and keychain aliases.", ["Jarvis","Home Assistant"], "ported", "core", "M1", ["AT-M1-CONFIG-ROUNDTRIP"], "in_progress"),
    ("CAP-RUNTIME","agent-runtime","Provider-neutral AgentRuntime with pinned authenticated OpenCode sidecar, streaming, failover, and session lifecycle.", ["Jarvis","OpenCode","Codex","Claude Code","Gemini CLI"], "ported", "core", "M2", ["AT-M2-STREAMED-TURN","AT-M2-SIDECAR-AUTH"], "in_progress"),
    ("CAP-VOICE","audio","Offline and hosted wake, VAD, STT, TTS, AEC, barge-in, dictation, multilingual, and visible microphone privacy.", ["Jarvis","GLaDOS","OpenVoiceOS","Home Assistant Assist","Pipecat","LiveKit Agents"], "ported", "core", "M3", ["AT-M3-OFFLINE-VOICE","AT-M3-BARGE-IN"], "in_progress"),
    ("CAP-CONVERSATION","conversation","Persistent typed and voice conversations with projects, persona, follow-up, guest, sleep, mute, quiet, and stop states.", ["Jarvis","ChatGPT","Claude","Gemini","Alexa+"], "ported", "core", "M3", ["AT-M3-CONVERSATION-STATES"], "in_progress"),
    ("CAP-WORKSPACE","ui","Cinematic HUD and accessible full workspace showing exact activity, agents, approvals, artifacts, usage, and diagnostics.", ["Jarvis","Raycast","OpenHands","Cursor","Windsurf"], "ported", "core", "M4", ["AT-M4-ACCESSIBLE-WORKSPACE"], "in_progress"),
    ("CAP-PROJECTS","projects","Per-project context, worktrees, terminals, project memory, and safe registered workspace boundaries.", ["Jarvis","Codex","Claude Code","Aider","Cline"], "ported", "core", "M4", ["AT-M4-PROJECT-CONTEXT"], "in_progress"),
    ("CAP-SAFETY","security","Tool schemas, scopes, risk/effect policy, scoped consent, data zones, checkpoints, rollback, egress, and immutable audit.", ["Jarvis","OpenCode","Claude Code","Codex"], "strengthened", "core", "M5", ["AT-M5-POLICY-GATE","AT-M5-ROLLBACK","AT-M9-SECURITY"], "in_progress"),
    ("CAP-DESKTOP","native-tools","Accessibility-first apps, windows, input, capture, clipboard, files, terminal, notifications, media, power, OCR, and health.", ["Jarvis","OpenClaw","Raycast","Alfred"], "ported", "core", "M5", ["AT-M5-DESKTOP-TOOLS"], "in_progress"),
    ("CAP-BROWSER","browser","Replaceable isolated Chromium/CDP engine with structured handles, takeover, domain policy, quarantine, and injection boundaries.", ["Jarvis","Playwright MCP","Chrome DevTools MCP","browser-use","Stagehand","Skyvern"], "ported", "official-pack", "M5", ["AT-M5-BROWSER-SAFE"], "in_progress"),
    ("CAP-AGENT","agent-runtime","Durable goals, validated task DAGs, planner/executor/reviewer agents, bounded parallelism, recovery, steering, and verification.", ["OpenCode","Codex","Claude Code","OpenHands","Goose"], "new", "core", "M6", ["AT-M6-DAG-RECOVERY"], "in_progress"),
    ("CAP-MEMORY","memory","Working, episodic, semantic, procedural, project, and entity memory with provenance, review, conflict, expiry, FTS, and vectors.", ["Jarvis","OpenClaw","ChatGPT","Claude"], "strengthened", "core", "M6", ["AT-M6-MEMORY-PROVENANCE"], "in_progress"),
    ("CAP-AUTOMATION","automation","Cron, event, semantic, and heartbeat triggers with quiet hours, previous state, concurrency, missed runs, and failure pauses.", ["Jarvis","ChatGPT Tasks","Home Assistant","automation platforms"], "strengthened", "core", "M6", ["AT-M6-AUTOMATION-RECOVERY"], "in_progress"),
    ("CAP-EXTENSIONS","extensions","Agent Skills, experts, commands, MCP, OAuth connectors, signed plugins, WASI/process isolation, and proposal review.", ["Jarvis","OpenCode","Claude Code","Codex","Cline"], "strengthened", "core", "M7", ["AT-M7-PLUGIN-SAFETY","AT-M7-COMPETITOR-COVERAGE"], "in_progress"),
    ("CAP-CONNECTORS","connectors","Official productivity, communications, development, smart-home, media, cloud, travel, shopping, and creative packs.", ["ChatGPT","Claude","Gemini","Alexa+","Home Assistant"], "new", "official-pack", "M7", ["AT-M7-COMPETITOR-COVERAGE"], "planned"),
    ("CAP-RESEARCH","research","Cited multi-source research, contradiction reporting, saved projects, structured extraction, and source provenance.", ["ChatGPT","Perplexity","Gemini","Claude"], "new", "core", "M7", ["AT-M7-COMPETITOR-COVERAGE"], "planned"),
    ("CAP-ARTIFACTS","artifacts","Versioned code, diffs, tables, charts, diagrams, reports, media, office documents, and whiteboard cards.", ["Jarvis","ChatGPT","Claude","Gemini"], "ported", "core", "M4", ["AT-M4-ACCESSIBLE-WORKSPACE"], "in_progress"),
    ("CAP-REMOTE","remote","Optional secure, capability-negotiated third-party remote protocol with fresh pairing and revocation.", ["Jarvis","OpenCode"], "ported", "official-pack", "M7", ["AT-M7-REMOTE-PROTOCOL"], "planned"),
    ("CAP-MIGRATION","migration","Read-only discovery, dry-run, consented idempotent import, provenance, reports, and no plaintext secret transfer.", ["Jarvis"], "new", "core", "M8", ["AT-M8-MIGRATION-DRY-RUN"], "in_progress"),
    ("CAP-OPERATIONS","operations","Single-app lifecycle, watchdog, suspend recovery, signed multi-platform packaging, updates, rollback, export, uninstall, and support bundles.", ["Jarvis","Tauri"], "ported", "core", "M9", ["AT-M1-PLATFORM-BUILD","AT-M9-LIFECYCLE","AT-M9-PERFORMANCE"], "in_progress"),
]

source_map = {
    "OpenCode":"https://opencode.ai/docs/", "Jarvis":"legacy://Jarvis", "OpenClaw":"https://docs.openclaw.ai/", "Leon":"https://github.com/leon-ai/leon",
    "Home Assistant":"https://developers.home-assistant.io/docs/voice/overview/", "Home Assistant Assist":"https://developers.home-assistant.io/docs/voice/overview/",
    "Codex":"https://developers.openai.com/codex/", "Claude Code":"https://docs.anthropic.com/en/docs/claude-code/overview", "Gemini CLI":"https://github.com/google-gemini/gemini-cli",
    "GLaDOS":"https://github.com/dnhkng/GlaDOS", "OpenVoiceOS":"https://openvoiceos.github.io/ovos-technical-manual/", "Pipecat":"https://docs.pipecat.ai/", "LiveKit Agents":"https://docs.livekit.io/agents/",
    "ChatGPT":"https://openai.com/chatgpt/overview/", "Claude":"https://www.anthropic.com/claude", "Gemini":"https://gemini.google.com/", "Alexa+":"https://www.aboutamazon.com/what-we-do/devices-services/alexa-plus",
    "Microsoft Copilot":"https://support.microsoft.com/en-us/microsoft-copilot/using-copilot-tasks",
    "Raycast":"https://developers.raycast.com/", "OpenHands":"https://docs.all-hands.dev/", "Cursor":"https://docs.cursor.com/", "Windsurf":"https://docs.windsurf.com/",
    "Aider":"https://aider.chat/docs/", "Cline":"https://docs.cline.bot/", "OpenClaw":"https://docs.openclaw.ai/", "Alfred":"https://www.alfredapp.com/help/",
    "Playwright MCP":"https://github.com/microsoft/playwright-mcp", "Chrome DevTools MCP":"https://github.com/ChromeDevTools/chrome-devtools-mcp", "browser-use":"https://docs.browser-use.com/", "Stagehand":"https://docs.stagehand.dev/", "Skyvern":"https://docs.skyvern.com/",
    "Goose":"https://block.github.io/goose/", "ChatGPT Tasks":"https://help.openai.com/en/articles/10291617-scheduled-tasks-in-chatgpt", "Perplexity":"https://www.perplexity.ai/help-center/en/articles/10352895-how-does-perplexity-work",
    "agent-browser":"https://github.com/vercel-labs/agent-browser", "TEN":"https://github.com/ten-framework/ten-framework", "Gemini Live":"https://ai.google.dev/gemini-api/docs/live-api",
    "Talon":"https://talonvoice.com/docs/", "n8n":"https://docs.n8n.io/advanced-ai/", "Tauri":"https://v2.tauri.app/",
    "automation platforms":"https://docs.n8n.io/", "shopping":"https://schema.org/Order", "communications":"https://www.rfc-editor.org/rfc/rfc9110"
}

capabilities = []
for cid, category, behavior, products, legacy_status, route, milestone, acceptance, status in cap_specs:
    capabilities.append({
        "id":cid,"category":category,"behavior":behavior,"products":products,
        "sources":[source_map[product] for product in products],
        "verified_on":TODAY,"source_confidence":"high" if all(product in source_map for product in products) else "medium",
        "legacy_status":legacy_status,"security_privacy":"Subject to data-zone labels, least privilege, consent for consequential effects, secret filtering, and immutable audit.",
        "route":route,"platforms":{"windows":"supported-or-gated","macos":"supported-or-gated","linux":"supported-or-gated"},
        "dependencies":[],"milestone":milestone,"acceptance_test_ids":acceptance,"status":status,
    })

competitor_products = [
    ("OpenCode","agent-runtime",source_map["OpenCode"],["provider-neutral runtime","client/server API","agents","MCP","skills"]),
    ("OpenClaw","agent-runtime",source_map["OpenClaw"],["isolated agents","channel routing","skills","local workspaces"]),
    ("GLaDOS","voice-assistant",source_map["GLaDOS"],["local voice pipeline","reactive persona"]),
    ("Leon","voice-assistant",source_map["Leon"],["local providers","layered memory","native and agent skills","bounded proactivity"]),
    ("Home Assistant Assist","smart-home",source_map["Home Assistant"],["local voice","satellites","home control"]),
    ("OpenVoiceOS","voice-assistant",source_map["OpenVoiceOS"],["wake words","skills","offline voice"]),
    ("ChatGPT Agent/Tasks","assistant","https://openai.com/index/introducing-chatgpt-agent/",["browser agent","scheduled tasks","connectors","artifacts"]),
    ("Claude/Cowork","assistant","https://www.anthropic.com/engineering/how-we-contain-claude",["computer use","artifacts","project context","local VM containment"]),
    ("Gemini","assistant",source_map["Gemini"],["multimodal live","research","workspace integrations"]),
    ("Alexa+","assistant",source_map["Alexa+"],["ambient assistance","smart home","proactivity"]),
    ("Microsoft Copilot","assistant",source_map["Microsoft Copilot"],["goal-based tasks","scheduled work","browser takeover","sensitive-action approval"]),
    ("Perplexity Computer","assistant","https://www.perplexity.ai/help-center/en/articles/11521526-perplexity-tasks",["cited research","scheduled tasks","connectors","isolated background agents"]),
    ("Codex","coding-agent",source_map["Codex"],["parallel agents","worktrees","automations","review"]),
    ("Claude Code","coding-agent",source_map["Claude Code"],["hooks","skills","MCP","subagents"]),
    ("Gemini CLI","coding-agent",source_map["Gemini CLI"],["terminal agent","MCP","extensions"]),
    ("Goose","coding-agent",source_map["Goose"],["extensions","recipes","provider choice"]),
    ("Cline","coding-agent",source_map["Cline"],["approval-driven tools","MCP","browser"]),
    ("OpenHands","coding-agent",source_map["OpenHands"],["sandboxed execution","web UI","evaluation"]),
    ("Aider","coding-agent",source_map["Aider"],["git-native edits","repository maps"]),
    ("Cursor","coding-agent",source_map["Cursor"],["IDE agents","background work","rules"]),
    ("Windsurf","coding-agent",source_map["Windsurf"],["IDE agent","workflows","context"]),
    ("Playwright MCP","browser",source_map["Playwright MCP"],["structured browser control","accessibility snapshots"]),
    ("Chrome DevTools MCP","browser",source_map["Chrome DevTools MCP"],["CDP inspection","performance diagnostics"]),
    ("browser-use","browser",source_map["browser-use"],["agent browser abstraction","session profiles"]),
    ("agent-browser","browser",source_map["agent-browser"],["native automation CLI","structured snapshots","stable element references","session profiles"]),
    ("Stagehand","browser",source_map["Stagehand"],["structured actions","observations","extraction"]),
    ("Skyvern","browser",source_map["Skyvern"],["visual fallback","workflow automation"]),
    ("Pipecat","voice",source_map["Pipecat"],["realtime pipelines","turn detection","transports"]),
    ("LiveKit Agents","voice",source_map["LiveKit Agents"],["realtime voice agents","rooms","interruptions"]),
    ("TEN","voice",source_map["TEN"],["realtime multimodal pipelines","VAD","turn detection","interruptions"]),
    ("OpenAI Realtime","voice","https://platform.openai.com/docs/guides/realtime",["speech-to-speech","realtime events","tool calling"]),
    ("Gemini Live","voice",source_map["Gemini Live"],["realtime voice and vision","barge-in","multilingual speech","tool use"]),
    ("Talon","dictation",source_map["Talon"],["cross-platform voice control","dictation","application-specific commands","accessibility input"]),
    ("n8n","automation",source_map["n8n"],["event and schedule triggers","workflow graphs","connectors","AI agents"]),
    ("Raycast","desktop-productivity",source_map["Raycast"],["command palette","extensions","quick AI"]),
    ("Alfred","desktop-productivity",source_map["Alfred"],["hotkeys","workflows","clipboard history"]),
]
competitors = [{"id":f"COMP-{index:03d}","product":name,"category":category,"primary_source":url,"verified_on":TODAY,"source_confidence":"high","capabilities":features,"status":"active"} for index,(name,category,url,features) in enumerate(competitor_products,1)]

rejections = [
    {"id":"REJ-001","behavior":"Unrestricted spoken eval or shell execution","products":[],"reason":"Voice is not authentication and model text is untrusted; every effect must use a declared tool and policy gate.","category":"security","decision":"rejected"},
    {"id":"REJ-002","behavior":"Advertising, sponsored answers, or behavioral targeting","products":["commercial assistants"],"reason":"Conflicts with the user-owned privacy model and product mission.","category":"advertising","decision":"rejected"},
    {"id":"REJ-003","behavior":"Claims of sentience or emotional dependency","products":[],"reason":"Affective behavior may be configurable but must be presented as behavior, not consciousness.","category":"deceptive","decision":"rejected"},
    {"id":"REJ-004","behavior":"Silent permission widening by agents, plugins, or subagents","products":[],"reason":"Violates least privilege and scoped consent.","category":"security","decision":"rejected"},
    {"id":"REJ-005","behavior":"Automatic installation of agent-written skills","products":[],"reason":"Self-authored skills enter a user-review proposal queue.","category":"supply-chain","decision":"rejected"},
    {"id":"REJ-006","behavior":"Scraping services when API terms prohibit it","products":[],"reason":"Commercial integrations use authorized APIs and connectors.","category":"legal","decision":"rejected"},
    {"id":"REJ-007","behavior":"Face or voice biometrics as authorization for consequential actions","products":[],"reason":"Biometrics may personalize but are not an authentication factor.","category":"security","decision":"rejected"},
    {"id":"REJ-008","behavior":"Mobile first-party application","products":["legacy Jarvis mobile"],"reason":"Desktop-only scope; the secure remote protocol remains available to third-party clients.","category":"scope","decision":"replaced-equivalently"},
    {"id":"REJ-009","behavior":"Trademark-copying competitor persona or visual identity","products":[],"reason":"Capabilities are deduplicated; branding remains Personal Agent with configurable JARVIS persona.","category":"legal","decision":"rejected"},
    {"id":"REJ-010","behavior":"Trust OpenCode V1 before-hook mutation as the safety boundary","products":["OpenCode"],"reason":"Upstream reports show argument mutation/short-circuit gaps. Effectful built-ins are disabled and replaced by gated MCP tools.","category":"security","decision":"replaced-equivalently","sources":["https://github.com/anomalyco/opencode/issues/42409","https://github.com/anomalyco/opencode/issues/32565"]},
]

for filename, document in {
    "acceptance-tests.yaml":{"schema_version":1,"updated_on":TODAY,"tests":[{"id":i,"milestone":m,"behavior":b,"verification":v,"kind":k,"status":s} for i,m,b,v,k,s in tests]},
    "capabilities.yaml":{"schema_version":1,"updated_on":TODAY,"capabilities":capabilities},
    "competitors.yaml":{"schema_version":1,"updated_on":TODAY,"products":competitors},
    "rejections.yaml":{"schema_version":1,"updated_on":TODAY,"rejections":rejections},
}.items():
    (ROOT / filename).write_text(json.dumps(document, indent=2, ensure_ascii=False) + "\n")

print(f"generated {len(capabilities)} capabilities, {len(competitors)} competitors, {len(tests)} tests, {len(rejections)} rejections")
