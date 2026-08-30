import { existsSync, readFileSync } from "node:fs";
import { isAbsolute, relative, resolve } from "node:path";
import { gzipSync } from "node:zlib";

const GZIP_LIMIT_BYTES = 300 * 1024;
const root = resolve(import.meta.dir, "..");
const dist = resolve(root, "apps/desktop/dist");
const indexPath = resolve(dist, "index.html");

if (!existsSync(indexPath)) {
  throw new Error(
    "desktop build output is missing; run `bun run --cwd apps/desktop build` first",
  );
}

const html = readFileSync(indexPath, "utf8");
const initialJavaScript = new Set<string>();
for (const match of html.matchAll(/(?:src|href)="([^"?#]+\.js)(?:[?#][^"]*)?"/g)) {
  initialJavaScript.add(match[1]!.replace(/^\//, ""));
}

if (initialJavaScript.size === 0) {
  throw new Error("desktop index.html does not reference an initial JavaScript chunk");
}

let gzipBytes = 0;
const details: string[] = [];
for (const relativePath of [...initialJavaScript].sort()) {
  const assetPath = resolve(dist, relativePath);
  const pathFromDist = relative(dist, assetPath);
  if (
    pathFromDist.startsWith("..") ||
    isAbsolute(pathFromDist) ||
    !existsSync(assetPath)
  ) {
    throw new Error(`initial JavaScript asset is missing: ${relativePath}`);
  }
  const compressedBytes = gzipSync(readFileSync(assetPath)).byteLength;
  gzipBytes += compressedBytes;
  details.push(`${relativePath}=${(compressedBytes / 1024).toFixed(2)} KiB`);
}

const summary = `${(gzipBytes / 1024).toFixed(2)} KiB gzip (${details.join(", ")})`;
if (gzipBytes >= GZIP_LIMIT_BYTES) {
  throw new Error(
    `initial desktop JavaScript is ${summary}; required limit is < 300.00 KiB gzip`,
  );
}

console.log(
  `Initial desktop JavaScript is ${summary}; limit < 300.00 KiB gzip: PASS`,
);
