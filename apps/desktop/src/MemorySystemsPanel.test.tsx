import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { MemorySystemsPanel } from "./MemorySystemsPanel";

const invoke = vi.fn();
vi.mock("@tauri-apps/api/core", () => ({ invoke: (...args: unknown[]) => invoke(...args) }));

describe("MemorySystemsPanel", () => {
  beforeEach(() => invoke.mockReset().mockResolvedValue({}));

  it("saves a reviewed writing preference", async () => {
    const onChanged = vi.fn().mockResolvedValue(undefined);
    render(<MemorySystemsPanel memories={[]} styles={[]} projects={{}} onChanged={onChanged} />);
    fireEvent.change(screen.getByPlaceholderText("Use concise headings and direct language"), { target: { value: "Prefer direct answers" } });
    fireEvent.click(screen.getByRole("button", { name: "Save reviewed preference" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("domain_action", {
      domain: "memory",
      action: "style_create",
      payload: { description: "Prefer direct answers", examples: [] },
    }));
    expect(onChanged).toHaveBeenCalled();
  });

  it("creates project graph nodes", async () => {
    const view = render(<MemorySystemsPanel memories={[]} styles={[]} projects={{}} onChanged={vi.fn().mockResolvedValue(undefined)} />);
    const ui = within(view.container);
    fireEvent.click(ui.getByRole("button", { name: "Projects" }));
    fireEvent.change(ui.getByLabelText("Name"), { target: { value: "Personal Agent" } });
    fireEvent.click(ui.getByRole("button", { name: "Add project node" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("domain_action", expect.objectContaining({
      domain: "memory",
      action: "project_node_create",
    })));
  });

  it("links conflicting facts", async () => {
    const memories = [
      { id: "one", content: "Use dark mode" },
      { id: "two", content: "Use light mode" },
    ];
    const view = render(<MemorySystemsPanel memories={memories} styles={[]} projects={{}} onChanged={vi.fn().mockResolvedValue(undefined)} />);
    const ui = within(view.container);
    fireEvent.click(ui.getByRole("button", { name: "Conflicts" }));
    const selects = ui.getAllByRole("combobox");
    fireEvent.change(selects[0]!, { target: { value: "one" } });
    fireEvent.change(selects[1]!, { target: { value: "two" } });
    fireEvent.click(ui.getByRole("button", { name: "Link conflict" }));
    await waitFor(() => expect(invoke).toHaveBeenCalledWith("domain_action", {
      domain: "memory",
      action: "link_conflict",
      payload: { left: "one", right: "two" },
    }));
  });
});
