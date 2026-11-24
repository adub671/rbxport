import { useEffect, useState } from "react";

import { useTranslation } from "@/i18n";
import { getBackend } from "@/ipc/client";
import { usePreferencesContext } from "@/store/usePreferences";
import { Button, Radios, Section, Select, Toggle } from "./controls";

/** Preferences › Import: dropped files are copied into a library folder before they are imported. */
export function ImportPane() {
  const t = useTranslation();
  const { preferences, update } = usePreferencesContext();
  const intake = preferences.intake;
  const [buildDefault, setBuildDefault] = useState(false);

  useEffect(() => {
    let live = true;
    void getBackend().then((b) => b.intakeDefaultEnabled()).then((on) => { if (live) setBuildDefault(on); });
    return () => { live = false; };
  }, []);

  const chooseFolder = () => void getBackend()
    .then((b) => b.pickFolder(t("Choose the library folder")))
    .then((root) => { if (root) update("intake", { root, copy: intake.copy ?? buildDefault }); }, () => undefined);

  const off = intake.root === "" || !(intake.copy ?? buildDefault);
  return (
    <Section title={t("Import")}>
      <Toggle label={t("Copy dropped files into the library folder")} checked={intake.copy ?? buildDefault}
        onChange={(copy) => update("intake", { copy })} />
      <div>
        <strong>{t("Library folder")}</strong>
        <p data-testid="intake-root">{intake.root === "" ? t("No folder chosen. Dropped files are imported where they are.") : intake.root}</p>
        <Button onClick={chooseFolder}>{t("Choose folder…")}</Button>
      </div>
      <Select label={t("Folder for new files")} caption={t("Folder for new files")} plain disabled={off} value={intake.layout}
        choices={[{ value: "month", label: t("A folder for each month (YYYY-MM)") }, { value: "flat", label: t("The library folder itself") }]}
        onChange={(layout) => update("intake", { layout })} />
      <Radios label={t("After import")} dim={off} value={intake.after}
        choices={[{ value: "keep", label: t("Keep originals") }, { value: "recycle", label: t("Move originals to the Recycle Bin") }]}
        onChange={(after) => update("intake", { after })} />
      <Toggle label={t("Treat the same name and size as a duplicate")} checked={intake.duplicates} disabled={off}
        onChange={(duplicates) => update("intake", { duplicates })} />
    </Section>
  );
}
