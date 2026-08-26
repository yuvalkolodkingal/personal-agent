import "@testing-library/jest-dom/vitest";
import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn().mockRejectedValue(new Error("browser test")) }));
import { App } from "./App";

describe("workspace", () => {
  it("exposes voice privacy and agent controls without relying on color", () => {
    render(<App />);
    expect(screen.getByRole("button", { name: "Push to talk" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Pause goal" })).toBeInTheDocument();
    expect(screen.getAllByText("WAKE-ONLY").length).toBeGreaterThan(0);
  });
});
