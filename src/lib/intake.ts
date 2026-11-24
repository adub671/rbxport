import type { IntakeProgress, IntakeReport, IntakeSettings } from "@/ipc/types";
import { formatBytes } from "@/lib/format";
import type { IntakePreferences } from "@/lib/preferences";

/**
 * The settings a drop is copied with, or `null` when Preferences › Import
 * leaves drops as upstream imports them: copying off, or no folder chosen.
 * Choosing the folder settles an unchosen copy switch to the build's default.
 */
export function intakeSettings(prefs: IntakePreferences): IntakeSettings | null {
  const { copy, ...settings } = prefs;
  return copy && settings.root.trim() !== "" ? settings : null;
}

type Translate = (text: string, values?: Readonly<Record<string, string | number>>) => string;

/** The status bar's "Copying 12 of 39 · 1.9 GB of 6.4 GB · 214.0 MB/s", `elapsedMs` into the copy. */
export function intakeLabel(progress: IntakeProgress, elapsedMs: number, t: Translate): string {
  const label = t("Copying {done} of {total} · {copied} of {size}", {
    done: progress.done, total: progress.total, copied: formatBytes(progress.bytes), size: formatBytes(progress.totalBytes),
  });
  return elapsedMs > 0 ? `${label} · ${formatBytes((progress.bytes * 1000) / elapsedMs)}/s` : label;
}

/** The report box's text: the folder, then each count and list that is not empty. */
export function intakeSummary(report: IntakeReport, t: Translate): string {
  const parts: [string, number | string[]][] = [
    [t("Copied"), report.copied + report.moved],
    [t("Converted to AIFF"), report.converted],
    [t("Imported"), report.imported],
    [t("Already in the library"), report.duplicates.length + report.existing.length],
    [t("Originals moved to the Recycle Bin"), report.recycled],
    [t("Renamed"), report.renamed],
    [t("Failed"), [...report.failed, ...report.skipped]],
    [t("Originals kept"), report.kept],
  ];
  const lines = parts.flatMap(([label, value]) => typeof value === "number"
    ? value > 0 ? [`${label}: ${value}.`] : []
    : value.length > 0 ? [`${label}: ${value.join("; ")}.`] : []);
  return [report.folder, ...lines].join(" ");
}
