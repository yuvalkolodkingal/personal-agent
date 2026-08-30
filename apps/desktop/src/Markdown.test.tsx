import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { MarkdownMessage, markdownSegments, safeLinkHref } from "./Markdown";

/**
 * These cases are the security boundary for assistant output. Every injection
 * fixture below is XSS-shaped on purpose: if a future dependency bump lets one
 * through, the snapshot diff is the alarm.
 */
const injections: Array<[name: string, source: string]> = [
  ["script tag", "Hello <script>alert('xss')</script> world"],
  ["image error handler", '<img src=x onerror="alert(1)">'],
  ["svg onload", "<svg/onload=alert(1)></svg>"],
  ["iframe", '<iframe src="https://evil.example/steal"></iframe>'],
  ["inline event handler", '<div onclick="alert(1)">click me</div>'],
  ["style import", "<style>@import url(https://evil.example/x.css)</style>"],
  ["javascript link", "[click me](javascript:alert(1))"],
  ["null byte javascript link", "[click me](java\u0000script:alert(1))"],
  ["tab split javascript link", "[click me](java\tscript:alert(1))"],
  ["uppercase javascript anchor", '<a href="JaVaScRiPt:alert(1)">go</a>'],
  ["data url link", "[open](data:text/html;base64,PHNjcmlwdD4=)"],
  ["remote tracking image", "![pixel](https://evil.example/pixel.png)"],
  ["form action", '<form action="https://evil.example"><input name="q"></form>'],
  ["object embed", '<object data="https://evil.example/x.swf"></object>'],
  ["base64 srcset", '<img srcset="https://evil.example/x 1x" alt="a">'],
  ["mixed case script", "<ScRiPt>alert(1)</ScRiPt>"],
  ["nested sanitiser escape", "<<script>script>alert(1)<</script>/script>"],
];

const markdownFixtures: Array<[name: string, source: string]> = [
  [
    "headings, emphasis, lists and quotes",
    [
      "# Title",
      "",
      "Some **bold**, some _italic_, some ~~struck~~ and `inline code`.",
      "",
      "- first",
      "- second",
      "  - nested",
      "",
      "1. one",
      "2. two",
      "",
      "> quoted claim",
      "",
      "| column | value |",
      "| --- | --- |",
      "| a | 1 |",
      "",
      "---",
      "",
      "[safe link](https://example.com/docs?a=1&b=2)",
    ].join("\n"),
  ],
  [
    "fenced code with language",
    ["Here is the fix:", "", "```rust", "fn main() {", '    println!("hi");', "}", "```", "", "Done."].join("\n"),
  ],
  [
    "fenced code without language",
    ["```", "plain block", "```"].join("\n"),
  ],
  [
    "code fence containing markup",
    ["```html", '<img src=x onerror="alert(1)">', "```"].join("\n"),
  ],
];

afterEach(() => cleanup());

describe("assistant markdown rendering", () => {
  it.each(markdownFixtures)("renders %s", (_name, source) => {
    expect(markdownSegments(source)).toMatchSnapshot();
  });

  it.each(injections)("sanitises %s", (_name, source) => {
    const segments = markdownSegments(source);
    expect(segments).toMatchSnapshot();
    const html = segments
      .map((segment) => (segment.kind === "html" ? segment.html : ""))
      .join("");
    // Escaped text such as `&lt;svg/onload=…&gt;` is inert, so the assertions
    // below run over the parsed DOM rather than the serialised string.
    for (const pattern of [
      /<script/i,
      /<iframe/i,
      /<img/i,
      /<svg/i,
      /<style/i,
      /<form/i,
      /<object/i,
      /<embed/i,
      /<input/i,
    ])
      expect(html).not.toMatch(pattern);
    expect(html).not.toMatch(/evil\.example\/steal/);

    const parsed = document.createElement("div");
    parsed.innerHTML = html;
    expect(
      parsed.querySelector(
        "script, iframe, img, svg, style, form, object, embed, input, link, base",
      ),
    ).toBeNull();
    for (const element of parsed.querySelectorAll("*"))
      for (const attribute of Array.from(element.attributes)) {
        expect(attribute.name.toLowerCase()).not.toMatch(/^on/);
        expect(attribute.value.toLowerCase()).not.toContain("javascript:");
        expect(attribute.value.toLowerCase()).not.toContain("data:text/html");
        expect(attribute.name.toLowerCase()).not.toBe("srcset");
      }
  });

  it("keeps the code fence content as inert text, never as markup", () => {
    const segments = markdownSegments(
      ['```html', '<img src=x onerror="alert(1)">', "```"].join("\n"),
    );
    expect(segments).toEqual([
      {
        kind: "code",
        key: "code-0",
        language: "html",
        code: '<img src=x onerror="alert(1)">',
      },
    ]);
  });

  it("rejects every destination that is not an absolute http, https or mailto URL", () => {
    expect(safeLinkHref("https://example.com/x")).toBe("https://example.com/x");
    expect(safeLinkHref("http://127.0.0.1:1420/a")).toBe(
      "http://127.0.0.1:1420/a",
    );
    expect(safeLinkHref("mailto:someone@example.com")).toBe(
      "mailto:someone@example.com",
    );
    expect(safeLinkHref("javascript:alert(1)")).toBeNull();
    expect(safeLinkHref("JAVASCRIPT:alert(1)")).toBeNull();
    expect(safeLinkHref("java\u0000script:alert(1)")).toBeNull();
    expect(safeLinkHref("data:text/html,<script>alert(1)</script>")).toBeNull();
    expect(safeLinkHref("vbscript:msgbox(1)")).toBeNull();
    expect(safeLinkHref("file:///etc/passwd")).toBeNull();
    expect(safeLinkHref("./relative/path")).toBeNull();
    expect(safeLinkHref("#anchor")).toBeNull();
    expect(safeLinkHref("")).toBeNull();
    expect(safeLinkHref(null)).toBeNull();
  });

  it("copies a single code block without touching the surrounding prose", async () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(
      <MarkdownMessage
        text={[
          "First:",
          "",
          "```ts",
          "const a = 1;",
          "```",
          "",
          "Second:",
          "",
          "```sh",
          "echo hi",
          "```",
        ].join("\n")}
      />,
    );
    expect(
      screen.getByRole("button", { name: "Copy ts code block" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: "Copy sh code block" }));
    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText).toHaveBeenCalledWith("echo hi");
    expect(
      await screen.findByRole("button", { name: "Copy sh code block" }),
    ).toHaveTextContent("Copied");
  });

  it("never navigates the local webview from an assistant link", () => {
    const writeText = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    render(<MarkdownMessage text="See [the docs](https://example.com/docs)." />);
    const link = screen.getByRole("link", { name: "the docs" });
    expect(link).toHaveAttribute("href", "https://example.com/docs");
    expect(link).toHaveAttribute("rel", "noopener noreferrer nofollow");
    expect(link).toHaveAttribute("target", "_blank");
    const defaultPrevented = !fireEvent.click(link);
    expect(defaultPrevented).toBe(true);
    expect(writeText).toHaveBeenCalledWith("https://example.com/docs");
  });

  it("names a markdown image instead of fetching a remote asset", () => {
    render(
      <MarkdownMessage text="![a diagram](https://cdn.example.com/d.png)" />,
    );
    expect(document.querySelector("img")).toBeNull();
    expect(
      screen.getByText(
        "[image: a diagram — https://cdn.example.com/d.png]",
      ),
    ).toBeInTheDocument();
  });

  it("renders the placeholder when the assistant has produced no text yet", () => {
    render(<MarkdownMessage text="" />);
    expect(screen.getByText("…")).toBeInTheDocument();
  });
});
