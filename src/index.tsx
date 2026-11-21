import {
  PanelSection,
  PanelSectionRow,
  staticClasses,
  Dropdown,
  SliderField,
  ToggleField,
  ButtonItem,
  TextField,
  showModal,
  ConfirmModal
} from "@decky/ui";
import {
  callable,
  toaster,
  definePlugin,
  fetchNoCors
} from "@decky/api"

import { useState, useEffect, useMemo } from "react";
import { FaGamepad } from "react-icons/fa";
import { t, tToast, setLanguage, formatDuration } from "./i18n";

const getGlobalConfig = callable<[], any>("get_config");
const saveGlobalConfig = callable<[config: any], boolean>("save_config");
const getSlotsData = callable<[], any>("get_slots_data");
const saveSlotsData = callable<[data: any], boolean>("save_slots_data");
const getDaemonState = callable<[], boolean>("get_daemon_state");
const setDaemonState = callable<[is_running: boolean], boolean>("set_daemon_state");
const getVersion = callable<[], string>("get_version");
const detectTrigger = callable<[which: string], { code?: number; label?: string; device?: string; error?: string }>("detect_trigger");
const getSlotStats = callable<[], Record<string, { events: number; duration_ms: number }>>("get_slot_stats");
const getMacroPreview = callable<[slotId: string], { count: number; duration_ms: number; rows: { t: number; input: string; action: string }[] }>("get_macro_preview");
const getDiagnostics = callable<[], { running: boolean; version: string; triggers: Record<string, any>; lines: string[] }>("get_diagnostics");
const getNotifications = callable<[], { message: string; error?: boolean }[]>("get_notifications");

// Back buttons: the game doesn't receive them (Steam's virtual pad has none).
const BACK_BUTTONS = [548, 549, 550, 551];
// Regular gamepad buttons: the game receives them too when used as triggers.
const GAME_BUTTONS = [304, 305, 307, 308, 310, 311, 312, 313, 314, 315, 317, 318, 544, 545, 546, 547];

const btnOptions = () => [
  { data: 317, label: t("btnL3") },
  { data: 318, label: t("btnR3") },
  { data: 310, label: t("btnL1") },
  { data: 311, label: t("btnR1") },
  { data: 314, label: t("btnSelect") },
  { data: 315, label: t("btnStart") },
  { data: 548, label: t("btnL4") },
  { data: 550, label: t("btnL5") },
  { data: 549, label: t("btnR4") },
  { data: 551, label: t("btnR5") },
];

// Adds the current code to the options when it was detected (e.g. a keyboard
// key) and isn't one of the listed buttons.
const withCustom = (code: number, label?: string) => {
  const opts = btnOptions();
  return opts.some((o) => o.data === code)
    ? opts
    : [...opts, { data: code, label: t("detected", { label: label || `key ${code}` }) }];
};

// Releases of this fork are published by .github/workflows/release.yml.
const UPDATE_REPO = "powange/decky-macro";
const UPDATE_ASSET = "decky-macro.zip";

// Compares "1.6.4-analog" style versions numerically (suffix ignored):
// > 0 when a is newer than b.
const compareVersions = (a: string, b: string) => {
  const parts = (v: string) => v.replace(/^v/, "").split("-")[0].split(".").map((n) => parseInt(n, 10) || 0);
  const pa = parts(a), pb = parts(b);
  for (let i = 0; i < Math.max(pa.length, pb.length); i++) {
    const d = (pa.at(i) ?? 0) - (pb.at(i) ?? 0);
    if (d !== 0) return d;
  }
  return 0;
};
// Must match "name" in plugin.json so Decky replaces the installed plugin.
const PLUGIN_NAME = "Decky Macro";
// Decky's PluginInstallType.UPDATE
const INSTALL_TYPE_UPDATE = 2;

const small = { fontSize: "11px", color: "#bbb", lineHeight: "1.3" } as const;

function UpdateSection() {
  const [current, setCurrent] = useState<string>("");
  const [latest, setLatest] = useState<{ tag: string; url: string } | null>(null);
  const [status, setStatus] = useState<string>("");
  const [busy, setBusy] = useState<boolean>(false);

  useEffect(() => {
    getVersion().then(setCurrent).catch(() => setCurrent("?"));
  }, []);

  const check = async () => {
    setBusy(true);
    setStatus(t("checking"));
    setLatest(null);
    try {
      const res = await fetchNoCors(`https://api.github.com/repos/${UPDATE_REPO}/releases/latest`, {
        headers: { Accept: "application/vnd.github+json" },
      });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const rel = await res.json();
      const asset = (rel.assets || []).find((a: any) => a.name === UPDATE_ASSET);
      if (!asset) throw new Error(`no ${UPDATE_ASSET} in the latest release`);
      if (compareVersions(rel.tag_name, current) <= 0) {
        setStatus(t("upToDate", { v: current }));
      } else {
        setLatest({ tag: rel.tag_name, url: asset.browser_download_url });
        setStatus(t("updateAvailable", { v: rel.tag_name }));
      }
    } catch (e: any) {
      setStatus(t("checkFailed", { e: e?.message ?? String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const install = async () => {
    if (!latest) return;
    const backend = (window as any).DeckyBackend;
    if (!backend?.call) {
      setStatus(t("noInstallApi", { url: `https://github.com/${UPDATE_REPO}/releases/latest/download/${UPDATE_ASSET}` }));
      return;
    }
    // Opens Decky's own install confirmation, like the store does.
    await backend.call(
      "utilities/install_plugin",
      latest.url,
      PLUGIN_NAME,
      latest.tag.replace(/^v/, ""),
      "",
      INSTALL_TYPE_UPDATE
    );
  };

  return (
    <PanelSection title={t("updates")}>
      <PanelSectionRow>
        <div style={{ fontSize: 12, color: "#bbb" }}>
          {t("installed")} <b>{current || "..."}</b>
          {status && <div>{status}</div>}
        </div>
      </PanelSectionRow>
      <PanelSectionRow>
        {latest ? (
          <ButtonItem layout="below" onClick={install}>
            {t("updateTo", { v: latest.tag })}
          </ButtonItem>
        ) : (
          <ButtonItem layout="below" onClick={check} disabled={busy}>
            {t("checkUpdates")}
          </ButtonItem>
        )}
      </PanelSectionRow>
    </PanelSection>
  );
}

// Module-level flag read by the notification poller, which runs on the plugin
// instance (outside <Content/>) and therefore has no access to the config state.
// Error toasts are always shown; info toasts honour this flag. <Content/> keeps
// it in sync with the `show_info_toasts` global config value.
let showInfoToasts = true;

const SlotEditModal = ({ slotId, initialData, slotsData, updateSlots, closeModal }: any) => {
  const [name, setName] = useState(initialData?.name || "");
  const [loops, setLoops] = useState(initialData?.loops || 0);
  const [speed, setSpeed] = useState<number>(initialData?.speed ?? 1);
  const [cancel, setCancel] = useState(initialData?.cancel_on_input ?? true);
  const [recordAnalog, setRecordAnalog] = useState(initialData?.record_analog ?? true);
  const [cardinalOnly, setCardinalOnly] = useState(initialData?.cardinal_only ?? false);

  return (
    <ConfirmModal
      onCancel={closeModal}
      onOK={() => {
        const newData = { ...slotsData, slots: { ...slotsData.slots } };
        newData.slots[slotId] = {
          name: name || t("unnamedSlot"),
          loops,
          speed,
          cancel_on_input: cancel,
          record_analog: recordAnalog,
          cardinal_only: cardinalOnly,
        };
        newData.active_slot = slotId;
        updateSlots(newData);
        closeModal();
      }}
      strTitle={t("configureSlot")}
    >
      <div style={{ padding: "0 0 20px 0" }}>
        <TextField
          label={t("slotName")}
          value={name}
          onChange={(e: any) => {
            if (e.target.value.length <= 30) {
              setName(e.target.value);
            }
          }}
        />
        <SliderField
          label={t("loopCount")}
          description={t("loopCountDesc")}
          value={loops}
          min={0}
          max={50}
          step={1}
          showValue={true}
          onChange={setLoops}
        />
        <SliderField
          label={t("speed")}
          description={t("speedDesc")}
          value={speed}
          min={0.5}
          max={2}
          step={0.25}
          showValue={true}
          valueSuffix="×"
          onChange={(v: number) => setSpeed(Math.round(v * 100) / 100)}
        />
        <ToggleField
          label={t("cancelOnInput")}
          description={t("cancelOnInputDesc")}
          checked={cancel}
          onChange={setCancel}
        />
        <ToggleField
          label={t("recordAnalog")}
          description={t("recordAnalogDesc")}
          checked={recordAnalog}
          onChange={setRecordAnalog}
        />
        <ToggleField
          label={t("cardinal")}
          description={t("cardinalDesc")}
          checked={cardinalOnly}
          disabled={!recordAnalog}
          onChange={setCardinalOnly}
        />
      </div>
    </ConfirmModal>
  );
};

const SettingsModal = ({ initialConfig, onSave, closeModal, daemonRunning }: any) => {
  const [recordCode, setRecordCode] = useState(initialConfig?.trigger_record_code ?? 317);
  const [replayCode, setReplayCode] = useState(initialConfig?.trigger_replay_code ?? 318);
  const holdMs = initialConfig?.hold_duration_ms ?? 500;
  const [recordLabel, setRecordLabel] = useState<string | undefined>(initialConfig?.trigger_record_label);
  const [replayLabel, setReplayLabel] = useState<string | undefined>(initialConfig?.trigger_replay_label);
  const [detecting, setDetecting] = useState<string>("");
  const [recordMode, setRecordMode] = useState<string>(initialConfig?.record_trigger_mode ?? "hold");
  const [replayMode, setReplayMode] = useState<string>(initialConfig?.replay_trigger_mode ?? "hold");
  const [recordCount, setRecordCount] = useState<number>(initialConfig?.record_press_count ?? 3);
  const [replayCount, setReplayCount] = useState<number>(initialConfig?.replay_press_count ?? 3);
  const pressWindowMs = initialConfig?.press_window_ms ?? 400;
  const [pickerCode, setPickerCode] = useState<number>(initialConfig?.trigger_picker_code ?? 0);
  const [pickerLabel, setPickerLabel] = useState<string | undefined>(initialConfig?.trigger_picker_label);
  const [pickerMode, setPickerMode] = useState<string>(initialConfig?.picker_trigger_mode ?? "hold");
  const [pickerCount, setPickerCount] = useState<number>(initialConfig?.picker_press_count ?? 3);
  const [language, setLang] = useState<string>(initialConfig?.language ?? "auto");
  // Per-trigger timings; the old shared values are the starting point.
  const sharedHold = initialConfig?.hold_duration_ms ?? 500;
  const sharedWindow = initialConfig?.press_window_ms ?? 400;
  const [recordHold, setRecordHold] = useState<number>(initialConfig?.record_hold_ms ?? sharedHold);
  const [replayHold, setReplayHold] = useState<number>(initialConfig?.replay_hold_ms ?? sharedHold);
  const [pickerHold, setPickerHold] = useState<number>(initialConfig?.picker_hold_ms ?? sharedHold);
  const [recordWindow, setRecordWindow] = useState<number>(initialConfig?.record_press_window_ms ?? sharedWindow);
  const [replayWindow, setReplayWindow] = useState<number>(initialConfig?.replay_press_window_ms ?? sharedWindow);
  const [pickerWindow, setPickerWindow] = useState<number>(initialConfig?.picker_press_window_ms ?? sharedWindow);

  const modeOptions = [
    { data: "hold", label: t("longPress") },
    { data: "multi_press", label: t("repeatedPresses") },
  ];
  const langOptions = [
    { data: "auto", label: t("langAuto") },
    { data: "en", label: "English" },
    { data: "fr", label: "Français" },
  ];

  const gameButtonWarning = (code: number) =>
    GAME_BUTTONS.includes(code) && !BACK_BUTTONS.includes(code) ? (
      <div style={{ ...small, color: "#ffcc00", margin: "4px 0" }}>{t("gameButtonWarning")}</div>
    ) : null;

  const triggerMode = (
    mode: string,
    setMode: (m: string) => void,
    count: number,
    setCount: (n: number) => void,
    holdMs: number,
    setHoldMs: (n: number) => void,
    windowMs: number,
    setWindowMs: (n: number) => void
  ) => (
    <>
      <Dropdown rgOptions={modeOptions} selectedOption={mode} onChange={(opt: any) => setMode(opt.data)} />
      {mode === "multi_press" ? (
        <>
          <SliderField
            label={t("pressesInRow")}
            value={count}
            min={2}
            max={10}
            step={1}
            showValue={true}
            onChange={(v: number) => setCount(Math.round(v))}
          />
          <SliderField
            label={t("maxBetween")}
            value={windowMs / 1000}
            min={0.2}
            max={1}
            step={0.05}
            showValue={true}
            valueSuffix="s"
            onChange={(v: number) => setWindowMs(Math.round(v * 1000))}
          />
        </>
      ) : (
        <SliderField
          label={t("holdTime")}
          description={t("holdTimeDesc")}
          value={holdMs / 1000}
          min={0}
          max={2}
          step={0.1}
          showValue={true}
          valueSuffix="s"
          onChange={(v: number) => setHoldMs(Math.round(v * 1000))}
        />
      )}
    </>
  );

  const detect = async (which: "record" | "replay" | "picker") => {
    setDetecting(which);
    try {
      const res = await detectTrigger(which);
      if (res?.code !== undefined) {
        if (which === "record") { setRecordCode(res.code); setRecordLabel(res.label); }
        else if (which === "picker") { setPickerCode(res.code); setPickerLabel(res.label); }
        else { setReplayCode(res.code); setReplayLabel(res.label); }
      }
    } finally {
      setDetecting("");
    }
  };

  const detectLabel = (which: string, idle: string) =>
    detecting === which ? t("detectNow") : daemonRunning ? t("detectStopFirst") : idle;

  return (
    <ConfirmModal
      onCancel={closeModal}
      onOK={() => {
        onSave({
          ...initialConfig,
          trigger_record_code: recordCode,
          trigger_replay_code: replayCode,
          trigger_record_label: recordLabel,
          trigger_replay_label: replayLabel,
          hold_duration_ms: holdMs,
          record_trigger_mode: recordMode,
          replay_trigger_mode: replayMode,
          record_press_count: recordCount,
          replay_press_count: replayCount,
          press_window_ms: pressWindowMs,
          trigger_picker_code: pickerCode,
          trigger_picker_label: pickerLabel,
          picker_trigger_mode: pickerMode,
          picker_press_count: pickerCount,
          record_hold_ms: recordHold,
          replay_hold_ms: replayHold,
          picker_hold_ms: pickerHold,
          record_press_window_ms: recordWindow,
          replay_press_window_ms: replayWindow,
          picker_press_window_ms: pickerWindow,
          language,
        });
        closeModal();
      }}
      strTitle={t("daemonSettings")}
    >
      <div style={{ padding: "0 0 20px 0" }}>
        <div style={{ ...small, marginBottom: "12px" }}>{t("settingsIntro")}</div>

        <div style={{ marginBottom: "5px" }}><b>{t("language")}</b></div>
        <Dropdown rgOptions={langOptions} selectedOption={language} onChange={(opt: any) => setLang(opt.data)} />

        <div style={{ marginTop: "12px", marginBottom: "5px" }}><b>{t("recordTrigger")}</b></div>
        <Dropdown
          rgOptions={withCustom(recordCode, recordLabel)}
          selectedOption={recordCode}
          onChange={(opt: any) => setRecordCode(opt.data)}
        />
        {gameButtonWarning(recordCode)}
        <ButtonItem layout="below" disabled={!!detecting || daemonRunning} onClick={() => detect("record")}>
          {detectLabel("record", t("detectRecord"))}
        </ButtonItem>
        {triggerMode(recordMode, setRecordMode, recordCount, setRecordCount, recordHold, setRecordHold, recordWindow, setRecordWindow)}

        <div style={{ marginTop: "12px", marginBottom: "5px" }}><b>{t("replayTrigger")}</b></div>
        <Dropdown
          rgOptions={withCustom(replayCode, replayLabel)}
          selectedOption={replayCode}
          onChange={(opt: any) => setReplayCode(opt.data)}
        />
        {gameButtonWarning(replayCode)}
        <ButtonItem layout="below" disabled={!!detecting || daemonRunning} onClick={() => detect("replay")}>
          {detectLabel("replay", t("detectReplay"))}
        </ButtonItem>
        {triggerMode(replayMode, setReplayMode, replayCount, setReplayCount, replayHold, setReplayHold, replayWindow, setReplayWindow)}

        <div style={{ marginTop: "12px", marginBottom: "5px" }}><b>{t("pickerTrigger")}</b></div>
        <div style={{ ...small, marginBottom: "5px" }}>{t("pickerDesc")}</div>
        <Dropdown
          rgOptions={[{ data: 0, label: t("disabled") }, ...withCustom(pickerCode || 317, pickerLabel)]}
          selectedOption={pickerCode}
          onChange={(opt: any) => setPickerCode(opt.data)}
        />
        {pickerCode !== 0 && (
          <>
            {gameButtonWarning(pickerCode)}
            <ButtonItem layout="below" disabled={!!detecting || daemonRunning} onClick={() => detect("picker")}>
              {detectLabel("picker", t("detectPicker"))}
            </ButtonItem>
            {triggerMode(pickerMode, setPickerMode, pickerCount, setPickerCount, pickerHold, setPickerHold, pickerWindow, setPickerWindow)}
          </>
        )}
      </div>
    </ConfirmModal>
  );
};

const InfoModal = ({ closeModal }: any) => (
  <ConfirmModal strTitle={t("infoTitle")} bAlertDialog={true} onOK={closeModal} onCancel={closeModal}>
    <div style={{ fontSize: "13px", lineHeight: "1.5", color: "#ccc" }}>
      <p>{t("info1")}</p>
      <p>{t("info2")}</p>
      <p>{t("info3")}</p>
    </div>
  </ConfirmModal>
);

const PREVIEW_ROWS = 300;

const PreviewModal = ({ slotId, name, closeModal }: any) => {
  const [data, setData] = useState<{ count: number; duration_ms: number; rows: { t: number; input: string; action: string }[] } | null>(null);
  useEffect(() => {
    getMacroPreview(slotId).then(setData).catch(() => setData({ count: 0, duration_ms: 0, rows: [] }));
  }, [slotId]);
  const cell = { padding: "2px 8px 2px 0", whiteSpace: "nowrap" } as const;
  return (
    <ConfirmModal strTitle={t("previewTitle", { name })} bAlertDialog={true} onOK={closeModal} onCancel={closeModal}>
      {!data ? (
        <div>{t("loading")}</div>
      ) : data.count === 0 ? (
        <div>{t("previewEmpty")}</div>
      ) : (
        <div>
          <div style={{ marginBottom: 8 }}>
            {t("previewSummary", { events: data.count, duration: formatDuration(data.duration_ms) })}
          </div>
          {data.count > PREVIEW_ROWS && <div style={small}>{t("previewTruncated", { n: PREVIEW_ROWS })}</div>}
          <div style={{ maxHeight: "50vh", overflowY: "auto", fontSize: 12 }}>
            <table style={{ borderCollapse: "collapse" }}>
              <thead>
                <tr style={{ color: "#bbb", textAlign: "left" }}>
                  <th style={cell}>{t("colTime")}</th>
                  <th style={cell}>{t("colInput")}</th>
                  <th style={cell}>{t("colAction")}</th>
                </tr>
              </thead>
              <tbody>
                {data.rows.map((r, i) => (
                  <tr key={i}>
                    <td style={cell}>{(r.t / 1000).toFixed(2)} s</td>
                    <td style={cell}>{r.input}</td>
                    <td style={cell}>{r.action}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </ConfirmModal>
  );
};

const DiagnosticModal = ({ closeModal }: any) => {
  const [diag, setDiag] = useState<{ running: boolean; version: string; triggers: Record<string, any>; lines: string[] } | null>(null);
  const load = () => getDiagnostics().then(setDiag).catch(() => {});
  useEffect(() => { load(); }, []);
  return (
    <ConfirmModal strTitle={t("diagTitle")} strOKButtonText={t("refresh")} onOK={load} onCancel={closeModal}>
      {!diag ? (
        <div>{t("loading")}</div>
      ) : (
        <div style={{ fontSize: 12, lineHeight: 1.4 }}>
          <div><b>{t("diagVersion")}:</b> {diag.version}</div>
          <div><b>{t("diagDaemon")}:</b> {diag.running ? t("running") : t("stopped")}</div>
          <div>
            <b>{t("diagTriggers")}:</b> {String(diag.triggers.record)} / {String(diag.triggers.replay)} /{" "}
            {diag.triggers.picker ? String(diag.triggers.picker) : t("disabled")}
          </div>
          <div style={{ marginTop: 8 }}><b>{t("diagLog")}</b></div>
          <div style={{ maxHeight: "45vh", overflowY: "auto", fontFamily: "monospace", fontSize: 10.5, whiteSpace: "pre-wrap", wordBreak: "break-all", background: "rgba(0,0,0,0.3)", padding: 6, borderRadius: 4 }}>
            {diag.lines.length ? diag.lines.map((l) => l.replace(/^\[[^\]]*\]\s*/, "")).join("\n") : t("diagNoLog")}
          </div>
        </div>
      )}
    </ConfirmModal>
  );
};

function Content() {
  const [globalConfig, setGlobalConfig] = useState<any>(null);
  const [slotsData, setSlotsData] = useState<any>(null);
  const [isDaemonRunning, setIsDaemonRunning] = useState<boolean>(false);
  const [stats, setStats] = useState<Record<string, { events: number; duration_ms: number }>>({});

  const refreshStats = () => getSlotStats().then(setStats).catch(() => {});

  useEffect(() => {
    getGlobalConfig().then((c) => {
      setLanguage(c?.language);
      setGlobalConfig(c);
    });
    getSlotsData().then(setSlotsData);
    getDaemonState().then(setIsDaemonRunning);
    refreshStats();
    // Recordings change the stats while the panel is open.
    const timer = setInterval(refreshStats, 3000);
    return () => clearInterval(timer);
  }, []);

  // Keep the module-level toast flag (read by the notification poller) in sync.
  useEffect(() => {
    showInfoToasts = globalConfig ? (globalConfig.show_info_toasts ?? true) : true;
  }, [globalConfig]);

  const setGlobal = async (newConfig: any) => {
    setLanguage(newConfig?.language);
    setGlobalConfig(newConfig);
    await saveGlobalConfig(newConfig);
  };

  const updateSlots = async (newData: any) => {
    setSlotsData(newData);
    await saveSlotsData(newData);
    refreshStats();
  };

  const toggleDaemon = async (state: boolean) => {
    // Optimistically set the UI toggle
    setIsDaemonRunning(state);
    try {
      await setDaemonState(state);
    } catch (e) {
      console.error("Daemon failed to start!", e);
      // Revert the toggle if the backend rejected the start command
      getDaemonState().then(setIsDaemonRunning);
    }
  };

  const handleNew = () => {
    if (!slotsData) return;
    const ids = Object.keys(slotsData.slots).map(Number);
    const newId = (ids.length > 0 ? Math.max(...ids) + 1 : 0).toString();
    showModal(
      <SlotEditModal
        slotId={newId}
        initialData={{ name: t("newSlotName", { id: newId }), loops: 0, speed: 1, cancel_on_input: true, record_analog: true, cardinal_only: false }}
        slotsData={slotsData}
        updateSlots={updateSlots}
      />
    );
  };

  const handleEdit = () => {
    if (!slotsData) return;
    const activeId = slotsData.active_slot;
    showModal(
      <SlotEditModal
        slotId={activeId}
        initialData={slotsData.slots[activeId]}
        slotsData={slotsData}
        updateSlots={updateSlots}
      />
    );
  };

  const handleDelete = () => {
    if (!slotsData) return;
    const activeId = slotsData.active_slot;
    if (activeId === "0") return; // Cannot delete default slot

    showModal(
      <ConfirmModal
        strTitle={t("deleteTitle")}
        strDescription={t("deleteDesc")}
        onOK={() => {
          const newData = { ...slotsData, slots: { ...slotsData.slots } };
          delete newData.slots[activeId];
          newData.active_slot = "0"; // Default back to slot 0
          updateSlots(newData);
        }}
      />
    );
  };

  const openSettings = () => {
    if (!globalConfig) return;
    showModal(
      <SettingsModal
        initialConfig={globalConfig}
        onSave={setGlobal}
        daemonRunning={isDaemonRunning}
      />
    );
  };

  const slotSummary = (id: string) => {
    const st = stats[id];
    if (!st || st.events === 0) return t("slotEmpty");
    return formatDuration(st.duration_ms);
  };

  // Memoize options so Dropdown doesn't lose focus / reset scroll position
  const slotOptions = useMemo(() => {
    if (!slotsData) return [];
    return Object.entries(slotsData.slots).map(([id, s]: [string, any]) => {
      return {
        data: id,
        label: (
          <div style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
            {s.name} <span style={{ color: "#999" }}>· {slotSummary(id)}</span>
          </div>
        )
      };
    });
  }, [slotsData, stats, globalConfig]);

  if (!globalConfig || !slotsData) return <div>{t("loading")}</div>;

  const activeSlotId = slotsData.active_slot;
  const active = slotsData.slots[activeSlotId] || {};
  const activeStats = stats[activeSlotId];
  const loops = Number(active.loops || 0);
  const speed = Number(active.speed ?? 1);

  return (
    <>
      <PanelSection>
        <PanelSectionRow>
          <ToggleField
            label={t("enable")}
            description={t("enableDesc")}
            checked={isDaemonRunning}
            onChange={(val: boolean) => toggleDaemon(val)}
          />
        </PanelSectionRow>
        <PanelSectionRow>
          <div style={{ padding: 10, backgroundColor: "rgba(0,0,0,0.3)", borderRadius: 4, fontSize: 13, lineHeight: "1.5" }}>
            <b>{t("status")} </b>
            {isDaemonRunning ? (
              <span style={{ color: "#00ffcc" }}>{t("running")}</span>
            ) : (
              <span style={{ color: "#ffcc00" }}>{t("stopped")}</span>
            )}
          </div>
        </PanelSectionRow>
      </PanelSection>

      <PanelSection>
        <PanelSectionRow>
          <ButtonItem layout="below" onClick={openSettings}>
            {t("settings")}
          </ButtonItem>
        </PanelSectionRow>
      </PanelSection>

      <PanelSection title={t("activeSlot")}>
        <PanelSectionRow>
          <Dropdown
            rgOptions={slotOptions}
            selectedOption={activeSlotId}
            onChange={(opt: any) => {
              setTimeout(() => updateSlots({ ...slotsData, active_slot: opt.data }), 0);
            }}
          />
        </PanelSectionRow>
        <PanelSectionRow>
          <div style={{ ...small, padding: "4px 2px" }}>
            {!activeStats || activeStats.events === 0
              ? t("slotEmpty")
              : t("slotInfo", { events: activeStats.events, duration: formatDuration(activeStats.duration_ms) })}
            {" · "}
            {loops === 0 ? t("loopsInfinite") : t("loopsN", { n: loops })}
            {speed !== 1 && <> · {t("speedInfo", { speed })}</>}
          </div>
        </PanelSectionRow>

        <PanelSectionRow>
          <div style={{ display: "flex", flexDirection: "column", gap: "5px", marginTop: "10px" }}>
            <ButtonItem layout="below" onClick={handleEdit}>
              {t("editSlot")}
            </ButtonItem>
            <ButtonItem
              layout="below"
              onClick={() => showModal(<PreviewModal slotId={activeSlotId} name={active.name || activeSlotId} />)}
              disabled={!activeStats || activeStats.events === 0}
            >
              {t("preview")}
            </ButtonItem>
            <ButtonItem
              layout="below"
              onClick={handleDelete}
              disabled={activeSlotId === "0"}
            >
              {t("deleteSlot")}
            </ButtonItem>
            <ButtonItem layout="below" onClick={handleNew}>
              {t("newSlot")}
            </ButtonItem>
          </div>
        </PanelSectionRow>
      </PanelSection>

      <PanelSection title={t("notifications")}>
        <PanelSectionRow>
          <ToggleField
            label={t("showToasts")}
            description={t("showToastsDesc")}
            checked={globalConfig.show_info_toasts ?? true}
            onChange={(val: boolean) => {
              setTimeout(() => setGlobal({ ...globalConfig, show_info_toasts: val }), 0);
            }}
          />
        </PanelSectionRow>
      </PanelSection>
      <PanelSection>
        <PanelSectionRow>
          <ButtonItem layout="below" onClick={() => showModal(<InfoModal />)}>
            {t("howItWorks")}
          </ButtonItem>
        </PanelSectionRow>
        <PanelSectionRow>
          <ButtonItem layout="below" onClick={() => showModal(<DiagnosticModal />)}>
            {t("diagnostic")}
          </ButtonItem>
        </PanelSectionRow>
      </PanelSection>
      <UpdateSection />
    </>
  );
}

export default definePlugin(() => {
  // Toasts can arrive before the panel is opened: pick the language now.
  getGlobalConfig().then((c) => setLanguage(c?.language)).catch(() => setLanguage("auto"));

  const interval = setInterval(async () => {
    try {
      const res = await getNotifications();
      if (res && res.length > 0) {
        for (const n of res) {
          const isError = !!n?.error;
          // Error toasts are always shown; info toasts honour the setting.
          if (!isError && !showInfoToasts) continue;
          toaster.toast({
            title: isError ? t("toastErrorTitle") : t("toastTitle"),
            body: tToast(n?.message ?? String(n)),
            duration: isError ? 4000 : 1500,
            sound: 0,
          });
        }
      }
    } catch (e) {
      // ignore
    }
  }, 1000);

  return {
    name: "Decky Macro",
    titleView: <div className={staticClasses.Title}>Decky Macro</div>,
    content: <Content />,
    icon: <FaGamepad />,
    onDismount() {
      clearInterval(interval);
    },
  };
});
