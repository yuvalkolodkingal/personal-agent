import type { ReactNode } from "react";

export function StatusPill({ tone = "neutral", children }: { tone?: "neutral" | "good" | "warn"; children: ReactNode }) {
  return <span className={`status-pill status-pill--${tone}`}>{children}</span>;
}
