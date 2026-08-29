import "@testing-library/jest-dom/vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));
import { ConnectorManager } from "./ConnectorManager";

const github = {
  id: "0198f5c8-d92e-7000-8000-000000000001",
  display_name: "Work GitHub",
  kind: "github",
  base_url: "https://api.github.com/",
  auth: { kind: "none" },
  grants: [
    { resource: "repositories", action: "read" },
    { resource: "issues", action: "read" },
  ],
  enabled: false,
};

describe("connector OAuth manager", () => {
  beforeEach(() => {
    invoke.mockReset();
    invoke.mockImplementation(async (command: string) => {
      if (command === "connector_list") return [github];
      if (command === "connector_oauth_authorize") {
        return { message: "Authorization completed." };
      }
      return {};
    });
  });

  afterEach(() => {
    cleanup();
    vi.restoreAllMocks();
  });

  it("starts GitHub with the exact reviewed read-only scopes and a public client ID", async () => {
    render(<ConnectorManager />);
    fireEvent.click(await screen.findByRole("button", { name: "Connect OAuth" }));
    fireEvent.change(screen.getByLabelText(/Public desktop client ID/), {
      target: { value: "public-github-client-id" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open secure sign-in" }));

    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("connector_oauth_authorize", {
        id: github.id,
        clientId: "public-github-client-id",
        scopes: ["read:user", "user:email"],
      }),
    );
    expect(await screen.findByRole("status")).toHaveTextContent("Authorization completed");
  });

  it("can cancel an in-flight loopback authorization without starting another", async () => {
    let finish: ((value: { message: string }) => void) | undefined;
    invoke.mockImplementation((command: string) => {
      if (command === "connector_list") return Promise.resolve([github]);
      if (command === "connector_oauth_authorize") {
        return new Promise<{ message: string }>((resolve) => {
          finish = resolve;
        });
      }
      return Promise.resolve(true);
    });
    render(<ConnectorManager />);
    fireEvent.click(await screen.findByRole("button", { name: "Connect OAuth" }));
    fireEvent.change(screen.getByLabelText(/Public desktop client ID/), {
      target: { value: "public-github-client-id" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Open secure sign-in" }));
    fireEvent.click(await screen.findByRole("button", { name: "Cancel authorization" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("connector_oauth_cancel", { id: github.id }),
    );
    expect(
      invoke.mock.calls.filter(([command]) => command === "connector_oauth_authorize"),
    ).toHaveLength(1);
    finish?.({ message: "Authorization completed." });
  });

  it("exposes refresh and confirmed revoke for a connected grant", async () => {
    const connected = {
      ...github,
      kind: "gmail",
      display_name: "Work Gmail",
      enabled: true,
      auth: {
        kind: "oauth2",
        account_label: "Google OAuth",
        client_id: "public-google-client-id",
        scopes: ["https://www.googleapis.com/auth/gmail.readonly"],
      },
    };
    invoke.mockImplementation(async (command: string) => {
      if (command === "connector_list") return [connected];
      return { message: "Done" };
    });
    vi.spyOn(window, "confirm").mockReturnValue(true);
    render(<ConnectorManager />);
    fireEvent.click(await screen.findByRole("button", { name: "Refresh OAuth" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("connector_oauth_refresh", { id: github.id }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Revoke OAuth" }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith("connector_oauth_revoke", {
        id: github.id,
        confirmed: true,
      }),
    );
  });
});
