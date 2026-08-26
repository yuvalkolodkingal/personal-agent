import { createHash } from "node:crypto";
import { chmodSync, copyFileSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join, resolve } from "node:path";

type Artifact = { name: string; sha256: string };
type Manifest = { version: string; artifacts: Record<string, Artifact> };

const root = resolve(import.meta.dir, "..");
const manifest = JSON.parse(readFileSync(resolve(root, "docs/operations/opencode-1.18.23.json"), "utf8")) as Manifest;
const requestedTarget = process.argv.find((argument) => argument.startsWith("--target="))?.slice("--target=".length);

const hostTarget = (() => {
  const arch = process.arch === "arm64" ? "aarch64" : process.arch === "x64" ? "x86_64" : process.arch;
  if (process.platform === "darwin") return `${arch}-apple-darwin`;
  if (process.platform === "win32") return `${arch}-pc-windows-msvc`;
  if (process.platform === "linux") return `${arch}-unknown-linux-gnu`;
  throw new Error(`unsupported build host ${process.platform}/${process.arch}`);
})();

const target = requestedTarget ?? hostTarget;
const artifactKey = (() => {
  const arch = target.startsWith("aarch64") ? "arm64" : target.startsWith("x86_64") ? "x64" : undefined;
  const platform = target.includes("apple-darwin") ? "darwin" : target.includes("windows-msvc") ? "windows" : target.includes("linux-gnu") ? "linux" : undefined;
  if (!arch || !platform) throw new Error(`unsupported OpenCode target ${target}`);
  return `${platform}-${arch}`;
})();

const artifact = manifest.artifacts[artifactKey];
if (!artifact) throw new Error(`manifest has no artifact for ${artifactKey}`);
const windows = target.includes("windows-msvc");
const destination = resolve(root, "apps/desktop/src-tauri/binaries", `opencode-${target}${windows ? ".exe" : ""}`);
const temporary = mkdtempSync(join(tmpdir(), "personal-agent-opencode-"));

function findBinary(directory: string): string | undefined {
  for (const entry of readdirSync(directory)) {
    const path = join(directory, entry);
    if (statSync(path).isDirectory()) {
      const nested = findBinary(path);
      if (nested) return nested;
    } else if (basename(path) === (windows ? "opencode.exe" : "opencode")) {
      return path;
    }
  }
  return undefined;
}

async function run(command: string[]): Promise<void> {
  const process = Bun.spawn(command, { stdout: "inherit", stderr: "inherit" });
  const exitCode = await process.exited;
  if (exitCode !== 0) throw new Error(`${command[0]} exited with ${exitCode}`);
}

async function capture(command: string[]): Promise<string> {
  const process = Bun.spawn(command, { stdout: "pipe", stderr: "pipe" });
  const stdout = await new Response(process.stdout).text();
  const stderr = await new Response(process.stderr).text();
  const exitCode = await process.exited;
  if (exitCode !== 0) throw new Error(`${command[0]} exited with ${exitCode}: ${stderr.trim()}`);
  return stdout.trim();
}

try {
  const archive = join(temporary, artifact.name);
  const url = `https://github.com/anomalyco/opencode/releases/download/v${manifest.version}/${artifact.name}`;
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) throw new Error(`download failed with HTTP ${response.status}: ${url}`);
  const bytes = new Uint8Array(await response.arrayBuffer());
  const actual = createHash("sha256").update(bytes).digest("hex");
  if (actual !== artifact.sha256) throw new Error(`checksum mismatch for ${artifact.name}: expected ${artifact.sha256}, found ${actual}`);
  await Bun.write(archive, bytes);

  const extracted = join(temporary, "extracted");
  mkdirSync(extracted);
  if (artifact.name.endsWith(".tar.gz")) {
    await run(["tar", "-xzf", archive, "-C", extracted]);
  } else if (process.platform === "win32") {
    await run(["powershell.exe", "-NoProfile", "-Command", `Expand-Archive -LiteralPath '${archive.replaceAll("'", "''")}' -DestinationPath '${extracted.replaceAll("'", "''")}' -Force`]);
  } else {
    await run(["unzip", "-q", archive, "-d", extracted]);
  }

  const binary = findBinary(extracted);
  if (!binary) throw new Error(`${artifact.name} did not contain an OpenCode executable`);
  mkdirSync(resolve(destination, ".."), { recursive: true });
  copyFileSync(binary, destination);
  if (!windows) chmodSync(destination, 0o755);
  const reportedVersion = await capture([destination, "--version"]);
  if (reportedVersion !== manifest.version) throw new Error(`executable version mismatch: expected ${manifest.version}, found ${reportedVersion}`);
  console.log(`verified OpenCode ${manifest.version} for ${target}: ${destination}`);
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
