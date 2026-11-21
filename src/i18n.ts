// Minimal i18n: English and French. The language comes from the "language"
// setting: "auto" follows the Steam UI language, "en" / "fr" force one.

export type Lang = "en" | "fr";

const en = {
  // panel
  enable: "Enable Decky Macro",
  enableDesc: "Stays on after a reboot if it was on",
  status: "Status:",
  running: "Running",
  stopped: "Stopped",
  settings: "Settings",
  activeSlot: "Active Macro Slot",
  editSlot: "Edit Selected Slot",
  deleteSlot: "Delete Selected Slot",
  newSlot: "New Slot",
  preview: "Preview macro",
  slotEmpty: "empty",
  slotInfo: "{events} events · {duration}",
  loopsInfinite: "loops ∞",
  loopsN: "{n} loops",
  speedInfo: "speed {speed}×",
  notifications: "Notifications",
  showToasts: "Show info toasts",
  showToastsDesc: "Show status toasts (recording started/saved, playback started/finished). Error toasts are always shown.",
  howItWorks: "ⓘ How it works",
  diagnostic: "Diagnostic",
  loading: "Loading...",
  // slot modal
  configureSlot: "Configure Slot",
  slotName: "Slot Name",
  unnamedSlot: "Unnamed Slot",
  newSlotName: "Slot {id}",
  loopCount: "Loop count",
  loopCountDesc: "How many times the replay should run (0 is infinite)",
  speed: "Playback speed",
  speedDesc: "1× = as recorded",
  cancelOnInput: "Cancel on input",
  cancelOnInputDesc: "Cancel replay if a button is pressed or a stick pushed past halfway",
  recordAnalog: "Record analog sticks",
  recordAnalogDesc: "Record sticks and L2/R2 triggers (in 25/50/75/100% steps). Applies to the next recording.",
  cardinal: "Sticks: 4 directions only",
  cardinalDesc: "Keep only the dominant axis of each stick (up, down, left or right). Slightly-up-left is recorded as pure left.",
  deleteTitle: "Delete Slot?",
  deleteDesc: "Delete this slot and its recorded macro?",
  // settings
  daemonSettings: "Daemon Settings",
  settingsIntro: "Changes apply right away, or as soon as the current recording or playback ends. Back buttons (L4/L5/R4/R5) are read directly from the Deck controller. Detect: press it, then press the button within 10 seconds (touches, A and B are ignored).",
  language: "Language",
  langAuto: "Auto (Steam language)",
  recordTrigger: "Record Trigger:",
  replayTrigger: "Replay Trigger:",
  pickerTrigger: "Macro Picker Trigger:",
  pickerDesc: "Shows the list of recorded macros in the middle of the screen: D-pad or left stick to choose, A to play, B to close.",
  disabled: "Disabled",
  detectRecord: "Detect record button",
  detectReplay: "Detect replay button",
  detectPicker: "Detect picker button",
  detectNow: "Press the button now (10 s)...",
  detectStopFirst: "Detect: stop the daemon first",
  detected: "Detected: {label}",
  gameButtonWarning: "The game also receives this button. Back buttons (L4/L5/R4/R5) avoid that.",
  longPress: "Long press",
  repeatedPresses: "Repeated presses",
  pressesInRow: "Presses in a row",
  maxBetween: "Max time between presses",
  holdTime: "Hold time",
  holdTimeDesc: "0 = trigger on press",
  // buttons
  btnL3: "L3 (Left Stick Click)",
  btnR3: "R3 (Right Stick Click)",
  btnL1: "L1 (Left Bumper)",
  btnR1: "R1 (Right Bumper)",
  btnSelect: "Select Button",
  btnStart: "Start Button",
  btnL4: "L4 (Back Upper Left)",
  btnL5: "L5 (Back Lower Left)",
  btnR4: "R4 (Back Upper Right)",
  btnR5: "R5 (Back Lower Right)",
  // info
  infoTitle: "How Decky Macro works",
  info1: "Decky Macro runs a daemon that listens to the gamepad to trigger and record macros, and injects them back into the same gamepad. It doesn't know whether the inputs go to a game or to the Steam client.",
  info2: "Analog sticks and triggers (L2/R2) are recorded in 4 steps per direction (25/50/75/100%), configurable per slot. During playback, pushing a stick past a quarter gives it back to you, past halfway or pressing a button cancels the macro (volume and power keys, the touchscreen and trackpads are ignored).",
  info3: "Back buttons (L4/L5/R4/R5) can be used as triggers. The optional macro picker trigger shows the list of recorded macros in the middle of the screen: D-pad or left stick to choose, A to play, B to close.",
  // preview
  previewTitle: "Macro: {name}",
  previewEmpty: "This slot has no recorded macro.",
  previewSummary: "{events} events · {duration}",
  previewTruncated: "Only the first {n} events are shown.",
  colTime: "Time",
  colInput: "Input",
  colAction: "Action",
  // diagnostic
  diagTitle: "Diagnostic",
  diagVersion: "Version",
  diagDaemon: "Daemon",
  diagTriggers: "Triggers (record / replay / picker)",
  diagLog: "Last daemon log lines",
  diagNoLog: "No log yet: enable Decky Macro.",
  refresh: "Refresh",
  // updates
  updates: "Updates",
  installed: "Installed:",
  checking: "Checking GitHub...",
  upToDate: "Up to date ({v})",
  updateAvailable: "Update available: {v}",
  checkFailed: "Check failed: {e}",
  noInstallApi: "Decky install API not available. Install from URL: {url}",
  checkUpdates: "Check for updates",
  updateTo: "Update to {v}",
  // toasts
  toastTitle: "Decky Macro",
  toastErrorTitle: "Decky Macro (error)",
};

export type Key = keyof typeof en;

const fr: Record<Key, string> = {
  enable: "Activer Decky Macro",
  enableDesc: "Reste activé après un redémarrage s'il l'était",
  status: "État :",
  running: "En marche",
  stopped: "Arrêté",
  settings: "Réglages",
  activeSlot: "Slot de macro actif",
  editSlot: "Modifier le slot",
  deleteSlot: "Supprimer le slot",
  newSlot: "Nouveau slot",
  preview: "Voir la macro",
  slotEmpty: "vide",
  slotInfo: "{events} événements · {duration}",
  loopsInfinite: "boucle ∞",
  loopsN: "{n} boucles",
  speedInfo: "vitesse {speed}×",
  notifications: "Notifications",
  showToasts: "Notifications d'état",
  showToastsDesc: "Affiche les notifications d'état (enregistrement démarré/sauvegardé, lecture démarrée/terminée). Les erreurs s'affichent toujours.",
  howItWorks: "ⓘ Comment ça marche",
  diagnostic: "Diagnostic",
  loading: "Chargement...",
  configureSlot: "Configurer le slot",
  slotName: "Nom du slot",
  unnamedSlot: "Slot sans nom",
  newSlotName: "Slot {id}",
  loopCount: "Nombre de boucles",
  loopCountDesc: "Combien de fois la macro est rejouée (0 = à l'infini)",
  speed: "Vitesse de lecture",
  speedDesc: "1× = comme enregistré",
  cancelOnInput: "Annuler si on touche la manette",
  cancelOnInputDesc: "Arrête la lecture si un bouton est pressé ou un stick poussé à plus de la moitié",
  recordAnalog: "Enregistrer les sticks",
  recordAnalogDesc: "Enregistre les sticks et les gâchettes L2/R2 (paliers 25/50/75/100 %). S'applique au prochain enregistrement.",
  cardinal: "Sticks : 4 directions seulement",
  cardinalDesc: "Ne garde que l'axe dominant de chaque stick (haut, bas, gauche ou droite). Gauche avec un peu de haut devient gauche pur.",
  deleteTitle: "Supprimer le slot ?",
  deleteDesc: "Supprimer ce slot et sa macro enregistrée ?",
  daemonSettings: "Réglages",
  settingsIntro: "Les changements s'appliquent tout de suite, ou dès la fin de l'enregistrement ou de la lecture en cours. Les palettes arrière (L4/L5/R4/R5) sont lues directement sur la manette du Deck. Détecter : appuie dessus, puis sur le bouton voulu dans les 10 secondes (l'écran tactile, A et B sont ignorés).",
  language: "Langue",
  langAuto: "Auto (langue de Steam)",
  recordTrigger: "Déclencheur d'enregistrement :",
  replayTrigger: "Déclencheur de lecture :",
  pickerTrigger: "Déclencheur du sélecteur de macro :",
  pickerDesc: "Affiche la liste des macros enregistrées au milieu de l'écran : croix ou stick gauche pour choisir, A pour lancer, B pour fermer.",
  disabled: "Désactivé",
  detectRecord: "Détecter le bouton d'enregistrement",
  detectReplay: "Détecter le bouton de lecture",
  detectPicker: "Détecter le bouton du sélecteur",
  detectNow: "Appuie sur le bouton (10 s)...",
  detectStopFirst: "Détecter : arrête d'abord le daemon",
  detected: "Détecté : {label}",
  gameButtonWarning: "Le jeu reçoit aussi ce bouton. Les palettes arrière (L4/L5/R4/R5) évitent ça.",
  longPress: "Appui long",
  repeatedPresses: "Appuis répétés",
  pressesInRow: "Nombre d'appuis",
  maxBetween: "Délai max entre deux appuis",
  holdTime: "Durée d'appui",
  holdTimeDesc: "0 = dès l'appui",
  btnL3: "L3 (clic du stick gauche)",
  btnR3: "R3 (clic du stick droit)",
  btnL1: "L1 (gâchette haute gauche)",
  btnR1: "R1 (gâchette haute droite)",
  btnSelect: "Bouton Vue (Select)",
  btnStart: "Bouton Menu (Start)",
  btnL4: "L4 (palette haut gauche)",
  btnL5: "L5 (palette bas gauche)",
  btnR4: "R4 (palette haut droite)",
  btnR5: "R5 (palette bas droite)",
  infoTitle: "Comment marche Decky Macro",
  info1: "Decky Macro lance un daemon qui écoute la manette pour déclencher et enregistrer des macros, puis les rejoue dans cette même manette. Il ne sait pas si les entrées vont à un jeu ou à l'interface de Steam.",
  info2: "Les sticks et les gâchettes (L2/R2) sont enregistrés par paliers de 25/50/75/100 % dans chaque direction, réglable par slot. Pendant la lecture, pousser un stick au-delà d'un quart te le rend, au-delà de la moitié (ou appuyer sur un bouton) annule la macro. Le volume, le bouton power, l'écran tactile et les trackpads sont ignorés.",
  info3: "Les palettes arrière (L4/L5/R4/R5) peuvent servir de déclencheurs. Le sélecteur de macro (optionnel) affiche la liste des macros au milieu de l'écran : croix ou stick gauche pour choisir, A pour lancer, B pour fermer.",
  previewTitle: "Macro : {name}",
  previewEmpty: "Ce slot n'a pas de macro enregistrée.",
  previewSummary: "{events} événements · {duration}",
  previewTruncated: "Seuls les {n} premiers événements sont affichés.",
  colTime: "Temps",
  colInput: "Entrée",
  colAction: "Action",
  diagTitle: "Diagnostic",
  diagVersion: "Version",
  diagDaemon: "Daemon",
  diagTriggers: "Déclencheurs (enregistrement / lecture / sélecteur)",
  diagLog: "Dernières lignes du daemon",
  diagNoLog: "Pas encore de log : active Decky Macro.",
  refresh: "Rafraîchir",
  updates: "Mises à jour",
  installed: "Installée :",
  checking: "Vérification sur GitHub...",
  upToDate: "À jour ({v})",
  updateAvailable: "Mise à jour disponible : {v}",
  checkFailed: "Échec de la vérification : {e}",
  noInstallApi: "API d'installation de Decky indisponible. Installe depuis l'URL : {url}",
  checkUpdates: "Vérifier les mises à jour",
  updateTo: "Mettre à jour vers {v}",
  toastTitle: "Decky Macro",
  toastErrorTitle: "Decky Macro (erreur)",
};

// Backend toast messages (English) and their French versions.
const toastsFr: Record<string, string> = {
  "Recording Started": "Enregistrement démarré",
  "Recording Saved": "Enregistrement sauvegardé",
  "No inputs recorded - nothing saved": "Aucune entrée enregistrée : rien n'a été sauvegardé",
  "Playback Started": "Lecture démarrée",
  "Playback Finished": "Lecture terminée",
  "Playback Cancelled": "Lecture annulée",
  "No macro recorded for this slot": "Aucune macro enregistrée dans ce slot",
  "No gamepad found to play the macro on": "Aucune manette trouvée pour jouer la macro",
  "Record button is not seen by the daemon - pick another one": "Le bouton d'enregistrement n'est pas vu par le daemon : choisis-en un autre",
  "Replay button is not seen by the daemon - pick another one": "Le bouton de lecture n'est pas vu par le daemon : choisis-en un autre",
  "Picker button is not seen by the daemon - pick another one": "Le bouton du sélecteur n'est pas vu par le daemon : choisis-en un autre",
  "Trigger button detected again": "Bouton déclencheur de nouveau détecté",
  "Macro size limit reached - recording saved": "Taille maximale atteinte : enregistrement sauvegardé",
  "No recorded macro to choose from": "Aucune macro enregistrée à choisir",
  "Macro picker: cannot read the gamepad": "Sélecteur : impossible de lire la manette",
  "Macro picker overlay could not start (see picker-overlay.log)": "Le sélecteur n'a pas pu s'afficher (voir picker-overlay.log)",
  "No button press detected": "Aucun appui détecté",
};

let current: Lang = "en";

/** Picks the UI language from the "language" setting. */
export function setLanguage(setting?: string) {
  if (setting === "en" || setting === "fr") {
    current = setting;
    return;
  }
  let steam = "";
  try {
    steam = (window as any).LocalizationManager?.m_rgLocalesToUse?.[0] ?? "";
  } catch {
    steam = "";
  }
  const lang = String(steam || navigator.language || "en").toLowerCase();
  current = lang.startsWith("fr") ? "fr" : "en";
}

export function getLanguage(): Lang {
  return current;
}

/** Translated string, with {placeholders} filled in. */
export function t(key: Key, params: Record<string, string | number> = {}): string {
  const table: Record<string, string> = current === "fr" ? fr : en;
  let s = table[key] ?? en[key] ?? key;
  for (const [k, v] of Object.entries(params)) {
    s = s.split(`{${k}}`).join(String(v));
  }
  return s;
}

/** Translates a toast message sent by the backend. */
export function tToast(message: string): string {
  if (current !== "fr") return message;
  if (toastsFr[message]) return toastsFr[message];
  const set = /^(Record|Replay|Picker) trigger set to (.+)$/.exec(message);
  if (set) {
    const which = { Record: "d'enregistrement", Replay: "de lecture", Picker: "du sélecteur" }[set[1] as "Record"];
    return `Déclencheur ${which} : ${set[2]}`;
  }
  return message;
}

/** "1.2 s", "2 min 05 s". */
export function formatDuration(ms: number): string {
  if (ms < 60000) return `${(ms / 1000).toFixed(1)} s`;
  const m = Math.floor(ms / 60000);
  const s = Math.round((ms % 60000) / 1000);
  return `${m} min ${String(s).padStart(2, "0")} s`;
}
