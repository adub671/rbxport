import { describe, expect, it } from "vitest";

import { DEFAULT_PREFERENCES, sanitisePreferences } from "@/lib/preferences";
import { intakeLabel, intakeSettings, intakeSummary } from "./intake";

const t = (text: string, values: Readonly<Record<string, string | number>> = {}) =>
  text.replace(/\{(\w+)\}/g, (_, key: string) => String(values[key]));

describe("intakeSettings", () => {
  const prefs = { ...DEFAULT_PREFERENCES.intake, root: "/Volumes/Music/Library" };

  it("stays off until a library folder is chosen and the switch is on", () => {
    expect(intakeSettings({ ...prefs, root: "", copy: true })).toBeNull();
    expect(intakeSettings({ ...prefs, root: "  ", copy: true })).toBeNull();
    expect(intakeSettings(prefs)).toBeNull();
    expect(intakeSettings({ ...prefs, copy: false })).toBeNull();
    expect(intakeSettings({ ...prefs, copy: true })).toEqual({ root: "/Volumes/Music/Library", layout: "month", after: "recycle", duplicates: true });
  });

  it("is kept through a round trip of stored preferences, and junk falls back", () => {
    const stored = sanitisePreferences({ intake: { copy: true, root: "/m", layout: "flat", after: "keep", duplicates: false } });
    expect(stored.intake).toEqual({ copy: true, root: "/m", layout: "flat", after: "keep", duplicates: false });
    expect(sanitisePreferences({ intake: { copy: "yes", root: 3, layout: "year" } }).intake).toEqual(DEFAULT_PREFERENCES.intake);
  });
});

describe("intakeLabel", () => {
  it("reads as the status bar line, with the average speed once time has passed", () => {
    const progress = { done: 12, total: 39, bytes: 2 * 1024 ** 3, totalBytes: 6.4 * 1024 ** 3 };
    expect(intakeLabel(progress, 0, t)).toBe("Copying 12 of 39 · 2.0 GB of 6.4 GB");
    expect(intakeLabel({ ...progress, bytes: 214 * 1024 ** 2 }, 1000, t)).toBe("Copying 12 of 39 · 214.0 MB of 6.4 GB · 214.0 MB/s");
  });
});

describe("intakeSummary", () => {
  it("names the folder and every count or list that is not empty", () => {
    const report = {
      imported: 2, skipped: ["cover.jpg: not audio"], tracks: [{ id: "1", title: "a.wav" }], existing: [],
      folder: "/m/2026-11", copied: 1, converted: 1, moved: 0, duplicates: ["c.mp3"], renamed: ["a.wav → a (2).wav"],
      failed: [], recycled: 1, kept: ["b.flac: no Recycle Bin"], cancelled: false,
    };
    expect(intakeSummary(report, t)).toBe("/m/2026-11 Copied: 1. Converted to AIFF: 1. Imported: 2. Already in the library: 1. "
      + "Originals moved to the Recycle Bin: 1. Renamed: a.wav → a (2).wav. Failed: cover.jpg: not audio. Originals kept: b.flac: no Recycle Bin.");
  });
});
