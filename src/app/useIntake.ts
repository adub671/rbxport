import { useCallback, useMemo } from "react";

import { useTranslation } from "@/i18n";
import { getBackend } from "@/ipc/client";
import type { IntakeReport } from "@/ipc/types";
import { intakeLabel, intakeSettings, intakeSummary } from "@/lib/intake";
import type { JobQueue } from "@/lib/jobQueue";
import type { IntakePreferences } from "@/lib/preferences";

/**
 * Preferences › Import's copy into the library folder, for the drops that
 * take it: files and folders dropped on All Tracks or a playlist. `start`
 * runs it as one stoppable status-bar job and shows the report when it ends;
 * a playlist drop then adds every track the drop stands for through
 * `addTracksTo`, as a drop without the copy does.
 */
export function useIntake({ prefs, jobs, onImported, addTracksTo }: {
  prefs: IntakePreferences;
  jobs: JobQueue;
  onImported: (tracks: IntakeReport["tracks"]) => void;
  addTracksTo: (playlistId: string, ids: readonly string[]) => void;
}) {
  const t = useTranslation();

  const settings = useMemo(() => intakeSettings(prefs), [prefs]);

  const start = useCallback((target: string, paths: string[]) => {
    if (!settings) return;
    jobs.add({
      label: t("Copying into the library folder…"),
      ...(target === "all" ? {} : { target }),
      run: async ({ signal, update }) => {
        const backend = await getBackend();
        const started = performance.now();
        const stopProgress = backend.onIntakeProgress((progress) => {
          update({ total: progress.total, done: progress.done, label: intakeLabel(progress, performance.now() - started, t) });
        });
        const cancel = () => void backend.cancelIntake();
        signal.addEventListener("abort", cancel);
        let done: IntakeReport;
        try {
          done = await backend.intakeAndImport(paths, settings);
        } finally {
          stopProgress();
          signal.removeEventListener("abort", cancel);
        }
        if (done.tracks.length > 0) onImported(done.tracks);
        const ids = [...done.tracks, ...done.existing].map((track) => track.id);
        if (target !== "all" && ids.length > 0) addTracksTo(target, ids);
        const track = done.tracks[0] ?? done.existing[0];
        void backend.confirm(intakeSummary(done, t), {
          title: done.cancelled ? t("Copy into the library folder stopped") : t("Copied into the library folder"),
          yes: t("Show in Explorer"), no: t("OK"),
        }).then((show) => { if (show && track) void backend.revealTrack(track.id); });
      },
    });
  }, [settings, jobs, t, onImported, addTracksTo]);

  /** What the drag-over overlay says: where files go, for which list, and what happens to the originals. */
  const note = useCallback((target: string) => settings
    ? `${t("Copies into {folder} for {target}", { folder: settings.root, target })} · ${
      settings.after === "recycle" ? t("Move originals to the Recycle Bin") : t("Keep originals")}`
    : undefined, [settings, t]);

  return { active: settings !== null, start, note };
}
