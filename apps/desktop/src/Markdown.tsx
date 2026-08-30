import { memo, useCallback, useEffect, useMemo, useState } from "react";
import DOMPurify, { type Config } from "dompurify";
import { Marked, type Token, type Tokens, type TokensList } from "marked";

/**
 * Assistant text is untrusted model output. It is rendered as markdown but only
 * after two independent boundaries:
 *
 * 1. `marked` renders with custom `link`/`image` renderers so no remote asset is
 *    ever referenced. The Tauri CSP is `default-src 'self'` with
 *    `img-src 'self' asset: http://asset.localhost data:` and no remote origin,
 *    so a remote `<img>` would be blocked anyway; dropping it here keeps the UI
 *    honest instead of showing a broken frame.
 * 2. `DOMPurify` sanitises the resulting HTML with an explicit tag/attribute
 *    allowlist. Raw HTML in markdown stays enabled on purpose so that this is
 *    the only place where it can be neutralised, and the sanitisation tests
 *    exercise it directly.
 *
 * Fenced code blocks never reach `innerHTML`: they are split out as tokens and
 * rendered as React text nodes with their own copy button.
 */

const ALLOWED_LINK_SCHEMES = new Set(["http:", "https:", "mailto:"]);

const ALLOWED_TAGS = [
  "a",
  "blockquote",
  "br",
  "code",
  "del",
  "em",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "li",
  "ol",
  "p",
  "pre",
  "span",
  "strong",
  "sub",
  "sup",
  "table",
  "tbody",
  "td",
  "th",
  "thead",
  "tr",
  "ul",
];

const ALLOWED_ATTR = [
  "class",
  "href",
  "rel",
  "start",
  "target",
  "title",
  "data-external-link",
];

// `USE_PROFILES` is deliberately absent: DOMPurify resets `ALLOWED_TAGS` to the
// whole profile when it is set, which would silently re-admit `img`/`svg`.
const PURIFY_OPTIONS: Config = {
  ALLOWED_TAGS,
  ALLOWED_ATTR,
  ALLOW_DATA_ATTR: false,
  ALLOW_ARIA_ATTR: false,
  ALLOW_UNKNOWN_PROTOCOLS: false,
  FORBID_TAGS: [
    "embed",
    "form",
    "iframe",
    "img",
    "input",
    "math",
    "object",
    "script",
    "style",
    "svg",
    "template",
    "textarea",
  ],
  FORBID_ATTR: ["formaction", "src", "srcset", "style", "xlink:href"],
  KEEP_CONTENT: true,
  RETURN_DOM: false,
  RETURN_DOM_FRAGMENT: false,
};

const HTML_ESCAPES: Record<string, string> = {
  "&": "&amp;",
  "<": "&lt;",
  ">": "&gt;",
  '"': "&quot;",
  "'": "&#39;",
};

function escapeHtml(value: string): string {
  return value.replace(/[&<>"']/g, (character) => HTML_ESCAPES[character]!);
}

/**
 * Resolve a markdown destination to an absolute `http(s)`/`mailto` URL, or
 * `null` when it must not become a link. Relative destinations are rejected:
 * the renderer has no document root to resolve them against and navigating the
 * local webview would tear down the app shell.
 */
export function safeLinkHref(raw: string | null | undefined): string | null {
  if (typeof raw !== "string") return null;
  const value = raw.trim();
  if (!value) return null;
  // Control characters (including NUL/newline/tab) are how `java\0script:` and
  // friends smuggle a scheme past naive prefix checks.
  if (/[\u0000-\u001f\u007f]/.test(value)) return null;
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  return ALLOWED_LINK_SCHEMES.has(url.protocol) ? url.href : null;
}

/**
 * `ALLOWED_URI_REGEXP` is deliberately left at the DOMPurify default: narrowing
 * it also rejects every non-URI attribute value (DOMPurify runs the same test
 * over `rel`, `target`, …). The stricter href policy is enforced here instead,
 * which additionally covers raw `<a>` tags the model may emit as HTML — those
 * never pass through the marked link renderer.
 */
DOMPurify.addHook("afterSanitizeAttributes", (node) => {
  const element = node as Element;
  if (typeof element.getAttribute !== "function") return;
  if (element.nodeName?.toLowerCase() !== "a") return;
  const target = safeLinkHref(element.getAttribute("href"));
  if (!target) {
    element.removeAttribute("href");
    element.removeAttribute("target");
    element.removeAttribute("rel");
    element.removeAttribute("data-external-link");
    return;
  }
  element.setAttribute("href", target);
  element.setAttribute("data-external-link", "true");
  element.setAttribute("target", "_blank");
  element.setAttribute("rel", "noopener noreferrer nofollow");
});

const markdown = new Marked({
  gfm: true,
  breaks: true,
  async: false,
  silent: true,
  renderer: {
    link({ href, title, tokens }: Tokens.Link) {
      const text = this.parser.parseInline(tokens);
      const target = safeLinkHref(href);
      if (!target)
        return `<span class="markdown-link-blocked" title="Blocked link destination">${text}</span>`;
      const escaped = escapeHtml(target);
      return `<a href="${escaped}" title="${escapeHtml(title ?? target)}" data-external-link="true" target="_blank" rel="noopener noreferrer nofollow">${text}</a>`;
    },
    image({ href, title, text }: Tokens.Image) {
      // No remote assets: the destination is named, never fetched.
      const label = escapeHtml(text || title || "image");
      const destination = escapeHtml(String(href ?? ""));
      return `<span class="markdown-image-blocked" title="Images are not fetched">[image: ${label} — ${destination}]</span>`;
    },
  },
});

export type MarkdownSegment =
  | { kind: "html"; key: string; html: string }
  | { kind: "code"; key: string; language: string; code: string };

/**
 * Split assistant markdown into sanitised HTML runs and fenced code blocks.
 * Exported so tests can assert on the boundary without rendering React.
 */
export function markdownSegments(source: string): MarkdownSegment[] {
  if (!source) return [];
  let tokens: TokensList;
  try {
    tokens = markdown.lexer(source);
  } catch {
    return [
      {
        kind: "html",
        key: "html-0",
        html: `<p>${escapeHtml(source)}</p>`,
      },
    ];
  }
  const segments: MarkdownSegment[] = [];
  let buffer: Token[] = [];
  const flush = () => {
    if (buffer.length === 0) return;
    const list = Object.assign([...buffer], {
      links: tokens.links,
    }) as TokensList;
    buffer = [];
    let rendered: string;
    try {
      rendered = markdown.parser(list);
    } catch {
      return;
    }
    const html = String(DOMPurify.sanitize(rendered, PURIFY_OPTIONS));
    if (html.trim())
      segments.push({ kind: "html", key: `html-${segments.length}`, html });
  };
  for (const token of tokens) {
    if (token.type === "code") {
      flush();
      const block = token as Tokens.Code;
      segments.push({
        kind: "code",
        key: `code-${segments.length}`,
        language: (block.lang ?? "").trim().split(/\s+/)[0] ?? "",
        code: block.text ?? "",
      });
      continue;
    }
    buffer.push(token);
  }
  flush();
  return segments;
}

function CodeBlock({ language, code }: { language: string; code: string }) {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1_500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const label = language || "text";
  return (
    <figure className="markdown-code" data-language={label}>
      <figcaption>
        <span>{label}</span>
        <button
          type="button"
          aria-label={`Copy ${label} code block`}
          onClick={() => {
            void navigator.clipboard?.writeText(code);
            setCopied(true);
          }}
        >
          {copied ? "Copied" : "Copy"}
        </button>
      </figcaption>
      <pre>
        <code>{code}</code>
      </pre>
    </figure>
  );
}

/**
 * Render assistant markdown. Links never navigate the local webview: the
 * renderer has no external-open IPC command, so a click surfaces the
 * destination by copying it instead of silently doing nothing.
 */
export const MarkdownMessage = memo(function MarkdownMessage({
  text,
}: {
  text: string;
}) {
  const segments = useMemo(() => markdownSegments(text), [text]);
  const onClick = useCallback((event: React.MouseEvent<HTMLDivElement>) => {
    const node = event.target as HTMLElement | null;
    const anchor = node?.closest?.("a[data-external-link]");
    if (!anchor) return;
    event.preventDefault();
    const href = anchor.getAttribute("href");
    if (href) void navigator.clipboard?.writeText(href);
  }, []);
  if (segments.length === 0) return <p>…</p>;
  return (
    <div className="markdown-body" onClick={onClick}>
      {segments.map((segment) =>
        segment.kind === "code" ? (
          <CodeBlock
            key={segment.key}
            language={segment.language}
            code={segment.code}
          />
        ) : (
          <div
            key={segment.key}
            className="markdown-prose"
            // Sanitised above by DOMPurify with an explicit allowlist.
            dangerouslySetInnerHTML={{ __html: segment.html }}
          />
        ),
      )}
    </div>
  );
});
