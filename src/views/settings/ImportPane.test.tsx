/** @vitest-environment jsdom */
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";

import { DEFAULT_PREFERENCES, type Preferences } from "@/lib/preferences";
import { PreferencesProvider } from "@/store/usePreferences";

const held = vi.hoisted(() => ({
  backend: { intakeDefaultEnabled: vi.fn(() => Promise.resolve(true)), pickFolder: vi.fn(() => Promise.resolve<string | null>("/Volumes/Music")) },
}));
vi.mock("@/ipc/client", () => ({ getBackend: () => Promise.resolve(held.backend) }));

import { ImportPane } from "./ImportPane";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean;
}

let host: HTMLDivElement;
let root: Root;
const update = vi.fn();

async function mount(intake: Preferences["intake"]) {
  await act(async () => {
    await Promise.resolve();
    root.render(
      <PreferencesProvider value={{ preferences: { ...DEFAULT_PREFERENCES, intake }, update, reset: () => {} }}>
        <ImportPane />
      </PreferencesProvider>,
    );
  });
}

const copySwitch = () => [...host.querySelectorAll("label")]
  .find((label) => label.textContent === "Copy dropped files into the library folder")
  ?.querySelector<HTMLInputElement>("input") ?? null;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  update.mockClear();
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

it("shows the build's default until chosen and keeps the options off until a folder is chosen", async () => {
  await mount(DEFAULT_PREFERENCES.intake);
  expect(copySwitch()?.checked).toBe(true);
  expect(host.querySelector('[data-testid="intake-root"]')?.textContent).toMatch(/No folder chosen/);
  expect(host.querySelector<HTMLSelectElement>("select")?.disabled).toBe(true);
});

it("stores a chosen folder, settling the switch to the build's default, and a chosen switch", async () => {
  await mount({ ...DEFAULT_PREFERENCES.intake, root: "/old" });
  expect(host.querySelector<HTMLSelectElement>("select")?.disabled).toBe(false);
  await act(async () => {
    [...host.querySelectorAll("button")].find((b) => b.textContent === "Choose folder…")?.click();
    await Promise.resolve();
  });
  expect(update).toHaveBeenCalledWith("intake", { root: "/Volumes/Music", copy: true });
  act(() => copySwitch()?.click());
  expect(update).toHaveBeenCalledWith("intake", { copy: false });
});
