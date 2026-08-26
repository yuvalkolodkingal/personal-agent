# Accessibility verification

The workspace uses native headings, forms, buttons, labels, lists, live regions, explicit status text, `aria-current`, `aria-pressed`, dialog semantics, visible focus, and non-color privacy states. Every destination is reachable through the sidebar and command palette. `Escape` closes the palette; Ctrl/Command-K opens it. Reduced-motion CSS removes all material animation.

Vitest exercises every navigation destination, palette keyboard operation, consent gating, exact unknown-event rendering, HUD state, theme labels, voice privacy, and disabled control semantics. The production build and typecheck pass.

Automated screenshot inspection could not run on the current Linux host because the Codex in-app browser runtime failed before connecting to the healthy Vite preview. This is an external visual-verification gap, not a passed screenshot result. CI/component semantics remain required, and a release operator must capture light/dark-equivalent theme baselines on Windows, macOS, and Linux before a production tag.
