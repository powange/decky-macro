use anyhow::Result;
use evdev::{Device, InputEvent, InputEventKind, Key};
use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const EV_ABS: u16 = 0x03;

// Analog axes captured from gamepads.
//  - ABS_X / ABS_Y      : left stick
//  - ABS_RX / ABS_RY    : right stick
//  - ABS_Z / ABS_RZ     : L2 / R2 on Steam's virtual (Xbox-style) gamepad
//  - ABS_HAT2X / HAT2Y  : R2 / L2 on the native hid-steam Steam Deck device
const ABS_X: u16 = 0x00;
const ABS_Y: u16 = 0x01;
const ABS_Z: u16 = 0x02;
const ABS_RX: u16 = 0x03;
const ABS_RY: u16 = 0x04;
const ABS_RZ: u16 = 0x05;
const ABS_HAT2X: u16 = 0x14;
const ABS_HAT0Y: u16 = 0x11;
const ABS_HAT2Y: u16 = 0x15;
const ANALOG_AXES: [u16; 8] = [ABS_X, ABS_Y, ABS_Z, ABS_RX, ABS_RY, ABS_RZ, ABS_HAT2X, ABS_HAT2Y];

// Max events per macro. Analog movement produces many more events than
// buttons, so the original 5000 limit was raised.
const MAX_EVENTS: usize = 20000;
// Deflection (fraction of the axis half-range) under which a physical stick
// movement is treated as drift (it won't redirect playback to that device).
const ANALOG_DEADZONE: f32 = 0.08;
// Analog axes are recorded in steps: ANALOG_LEVELS per direction
// (4 => 25/50/75/100%). Only level changes are stored, so a full stick sweep
// produces a handful of events instead of hundreds.
const ANALOG_LEVELS: i32 = 4;
// Hysteresis (in levels) so a stick held right on a boundary doesn't flap
// between two levels: a level change needs to go 0.5 + this past the current one.
// From rest this means ~17.5% to start moving and ~7.5% to come back to rest.
const LEVEL_HYSTERESIS: f32 = 0.2;
// During playback, a physical analog deflection above this fraction cancels
// the macro (when "cancel on input" is enabled). Small bumps are ignored.
const ANALOG_CANCEL_THRESHOLD: f32 = 0.5;

// Left / right stick axis pairs, used by the "4 directions" recording mode.
const STICK_PAIRS: [(u16, u16); 2] = [(ABS_X, ABS_Y), (ABS_RX, ABS_RY)];
// In "4 directions" mode the dominant axis only changes once the other axis is
// pushed further by at least this many levels (avoids flapping on diagonals).
const DOMINANCE_MARGIN: f32 = 0.5;
// Echo-cancellation entries older than this are dropped: the kernel does not
// echo an injected event whose value equals the current state, and a stale
// entry would otherwise swallow a later real press (e.g. the one meant to
// cancel a loop).
const ECHO_TTL: Duration = Duration::from_millis(250);

/// Human-readable name for the trigger key codes offered in the UI.
fn key_label(code: u16) -> String {
    match code {
        310 => "L1".into(),
        311 => "R1".into(),
        314 => "Select".into(),
        315 => "Start".into(),
        317 => "L3".into(),
        318 => "R3".into(),
        0x224 => "L4 (BTN_GRIPL)".into(),
        0x225 => "R4 (BTN_GRIPR)".into(),
        0x226 => "L5 (BTN_GRIPL2)".into(),
        0x227 => "R5 (BTN_GRIPR2)".into(),
        c => format!("{:?}", Key::new(c)),
    }
}

/// System keys (volume, power, brightness...): never cancel a macro and are
/// never recorded. On the Deck the volume buttons are a separate keyboard device.
const SYSTEM_KEYS: [u16; 8] = [
    113, // KEY_MUTE
    114, // KEY_VOLUMEDOWN
    115, // KEY_VOLUMEUP
    116, // KEY_POWER
    142, // KEY_SLEEP
    143, // KEY_WAKEUP
    224, // KEY_BRIGHTNESSDOWN
    225, // KEY_BRIGHTNESSUP
];

fn is_system_key(code: u16) -> bool {
    SYSTEM_KEYS.contains(&code)
}

fn stick_partner(code: u16) -> Option<(u16, u16)> {
    STICK_PAIRS.iter().copied().find(|(a, b)| *a == code || *b == code)
}

fn is_analog_axis(code: u16) -> bool {
    ANALOG_AXES.contains(&code)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct AxisRange {
    min: i32,
    max: i32,
}

impl AxisRange {
    /// Default ranges, used when the kernel didn't report one for the device.
    fn fallback(code: u16) -> Self {
        match code {
            ABS_Z | ABS_RZ | ABS_HAT2X | ABS_HAT2Y => AxisRange { min: 0, max: 255 },
            _ => AxisRange { min: -32768, max: 32767 },
        }
    }

    /// Resting value: centre for bipolar axes (sticks), minimum for triggers.
    fn rest(&self) -> i32 {
        if self.min < 0 && self.max > 0 { 0 } else { self.min }
    }

    /// How far `v` is from rest, as a fraction (0.0 = rest, 1.0 = full deflection).
    fn deflection(&self, v: i32) -> f32 {
        let r = self.rest();
        let span = if v >= r { self.max - r } else { r - self.min };
        if span <= 0 { 0.0 } else { ((v - r).abs() as f32 / span as f32).min(1.0) }
    }

    /// Signed position in levels (-ANALOG_LEVELS..=ANALOG_LEVELS), unrounded.
    fn level_position(&self, v: i32) -> f32 {
        let d = self.deflection(v) * ANALOG_LEVELS as f32;
        if v < self.rest() { -d } else { d }
    }

    /// Nearest level, without hysteresis.
    fn level(&self, v: i32) -> i32 {
        (self.level_position(v).round() as i32).clamp(-ANALOG_LEVELS, ANALOG_LEVELS)
    }

    /// Nearest level, but sticks to `prev` unless clearly past the boundary.
    fn level_from(&self, v: i32, prev: i32) -> i32 {
        let x = self.level_position(v);
        if (x - prev as f32).abs() < 0.5 + LEVEL_HYSTERESIS { prev } else { self.level(v) }
    }

    /// Axis value for a level (level ±ANALOG_LEVELS gives exactly min/max).
    fn level_value(&self, level: i32) -> i32 {
        let r = self.rest() as i64;
        let l = level.clamp(-ANALOG_LEVELS, ANALOG_LEVELS) as i64;
        let n = ANALOG_LEVELS as i64;
        let v = if l >= 0 {
            r + (self.max as i64 - r) * l / n
        } else {
            r - (r - self.min as i64) * (-l) / n
        };
        v as i32
    }
}

fn default_hold_ms() -> u64 {
    500
}

#[derive(Serialize, Deserialize, Clone, Debug)]
struct Config {
    trigger_record_code: u16,
    trigger_replay_code: u16,
    // How long the trigger button must be held before activating, in milliseconds.
    // 0 means "activate immediately on press". Defaults to 500 (0.5s) so that
    // config files written before this field existed keep their old behaviour.
    #[serde(default = "default_hold_ms")]
    hold_duration_ms: u64,
    // How each trigger activates: held for hold_duration_ms, or pressed
    // N times in a row (each press within press_window_ms of the previous).
    #[serde(default)]
    record_trigger_mode: TriggerMode,
    #[serde(default)]
    replay_trigger_mode: TriggerMode,
    #[serde(default = "default_press_count")]
    record_press_count: u32,
    #[serde(default = "default_press_count")]
    replay_press_count: u32,
    #[serde(default = "default_press_window_ms")]
    press_window_ms: u64,
    // Macro picker trigger (0 = disabled).
    #[serde(default)]
    trigger_picker_code: u16,
    #[serde(default)]
    picker_trigger_mode: TriggerMode,
    #[serde(default = "default_press_count")]
    picker_press_count: u32,
    // Per-trigger hold time / press window. Unset = the shared value above
    // (hold_duration_ms / press_window_ms), so older config files keep working.
    #[serde(default)]
    record_hold_ms: Option<u64>,
    #[serde(default)]
    replay_hold_ms: Option<u64>,
    #[serde(default)]
    picker_hold_ms: Option<u64>,
    #[serde(default)]
    record_press_window_ms: Option<u64>,
    #[serde(default)]
    replay_press_window_ms: Option<u64>,
    #[serde(default)]
    picker_press_window_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Trigger {
    Record,
    Replay,
    Picker,
}

impl Config {
    fn hold(&self, t: Trigger) -> Duration {
        let v = match t {
            Trigger::Record => self.record_hold_ms,
            Trigger::Replay => self.replay_hold_ms,
            Trigger::Picker => self.picker_hold_ms,
        };
        Duration::from_millis(v.unwrap_or(self.hold_duration_ms))
    }

    fn press_window(&self, t: Trigger) -> Duration {
        let v = match t {
            Trigger::Record => self.record_press_window_ms,
            Trigger::Replay => self.replay_press_window_ms,
            Trigger::Picker => self.picker_press_window_ms,
        };
        Duration::from_millis(v.unwrap_or(self.press_window_ms))
    }
}

fn default_press_count() -> u32 {
    3
}

fn default_press_window_ms() -> u64 {
    400
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "snake_case")]
enum TriggerMode {
    #[default]
    Hold,
    MultiPress,
}

/// Counts consecutive presses of a trigger button for TriggerMode::MultiPress.
#[derive(Default)]
struct PressCounter {
    count: u32,
    last: Option<Instant>,
}

impl PressCounter {
    /// Registers a press; returns true when it completes the series.
    fn press(&mut self, now: Instant, window: Duration, needed: u32) -> bool {
        let in_series = self.last.map_or(false, |t| now.duration_since(t) <= window);
        self.count = if in_series { self.count + 1 } else { 1 };
        self.last = Some(now);
        if self.count >= needed.max(1) {
            self.count = 0;
            self.last = None;
            true
        } else {
            false
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct MacroEvent {
    delay_ms: u64,
    event_type: u16,
    code: u16,
    value: i32,
}

impl MacroEvent {
    fn new(delay: Duration, event_type: u16, code: u16, value: i32) -> Self {
        Self { delay_ms: delay.as_millis() as u64, event_type, code, value }
    }

    fn from_evdev(delay: Duration, event: InputEvent) -> Self {
        Self::new(delay, event.event_type().0, event.code(), event.value())
    }
}

// Convert compressed string back to full struct for logic
fn parse_compressed_events(strings: Vec<String>) -> Vec<MacroEvent> {
    let mut events = Vec::new();
    for s in strings {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() == 4 {
            if let (Ok(d), Ok(t), Ok(c), Ok(v)) = (
                parts[0].parse::<u64>(),
                parts[1].parse::<u16>(),
                parts[2].parse::<u16>(),
                parts[3].parse::<i32>(),
            ) {
                events.push(MacroEvent {
                    delay_ms: d,
                    event_type: t,
                    code: c,
                    value: v,
                });
            }
        }
    }
    events
}

fn load_or_create_config(path: &str) -> Config {
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(config) = serde_json::from_str(&data) {
            return config;
        }
    }
    Config {
        trigger_record_code: 317,
        trigger_replay_code: 318,
        hold_duration_ms: default_hold_ms(),
        record_trigger_mode: TriggerMode::Hold,
        replay_trigger_mode: TriggerMode::Hold,
        record_press_count: default_press_count(),
        replay_press_count: default_press_count(),
        press_window_ms: default_press_window_ms(),
        trigger_picker_code: 0,
        picker_trigger_mode: TriggerMode::Hold,
        picker_press_count: default_press_count(),
        record_hold_ms: None,
        replay_hold_ms: None,
        picker_hold_ms: None,
        record_press_window_ms: None,
        replay_press_window_ms: None,
        picker_press_window_ms: None,
    }
}

fn load_macro(path: &Path) -> Vec<MacroEvent> {
    if let Ok(data) = fs::read_to_string(path) {
        if let Ok(strings) = serde_json::from_str::<Vec<String>>(&data) {
            return parse_compressed_events(strings);
        }
    }
    Vec::new()
}

fn save_macro(path: &Path, events: &Vec<MacroEvent>) {
    let mut compressed = Vec::new();
    for e in events {
        compressed.push(format!("{}:{}:{}:{}", e.delay_ms, e.event_type, e.code, e.value));
    }
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string(&compressed) {
        let _ = fs::write(path, json);
    }
}

/// Records analog axis movement in steps (see ANALOG_LEVELS).
/// With `cardinal`, each stick keeps only its dominant axis (4 directions).
struct AnalogRecorder {
    cardinal: bool,
    /// Last recorded level per axis (0 = rest).
    last_level: HashMap<u16, i32>,
    /// Latest physical value per axis.
    raw: HashMap<u16, (i32, AxisRange)>,
    /// Dominant axis per stick pair (keyed by the pair's first axis).
    dominant: HashMap<u16, u16>,
}

impl AnalogRecorder {
    fn new(cardinal: bool) -> Self {
        Self { cardinal, last_level: HashMap::new(), raw: HashMap::new(), dominant: HashMap::new() }
    }

    /// Starts a recording. Axes already deflected (e.g. stick held while the
    /// record trigger fired) are written as initial events at t=0.
    fn start(&mut self, current: &HashMap<u16, (i32, AxisRange)>) -> Vec<MacroEvent> {
        self.last_level.clear();
        self.dominant.clear();
        self.raw = current.clone();
        let mut codes: Vec<u16> = current.keys().copied().collect();
        codes.sort();
        let mut initial = Vec::new();
        for code in codes {
            initial.extend(self.update(Duration::ZERO, code));
        }
        initial
    }

    /// Feeds one physical value; returns the events to store (often none).
    fn record(&mut self, elapsed: Duration, code: u16, value: i32, range: AxisRange) -> Vec<MacroEvent> {
        self.raw.insert(code, (value, range));
        self.update(elapsed, code)
    }

    #[cfg(test)]
    fn record1(&mut self, elapsed: Duration, code: u16, value: i32, range: AxisRange) -> Option<MacroEvent> {
        let v = self.record(elapsed, code, value, range);
        assert!(v.len() <= 1, "expected at most one event, got {:?}", v);
        v.into_iter().next()
    }

    fn position(&self, code: u16) -> f32 {
        self.raw.get(&code).map_or(0.0, |(v, r)| r.level_position(*v))
    }

    fn target_level(&self, code: u16) -> i32 {
        let last = *self.last_level.get(&code).unwrap_or(&0);
        self.raw.get(&code).map_or(0, |(v, r)| r.level_from(*v, last))
    }

    fn set_level(&mut self, out: &mut Vec<MacroEvent>, elapsed: Duration, code: u16, level: i32) {
        let last = *self.last_level.get(&code).unwrap_or(&0);
        if level == last {
            return;
        }
        let range = self.raw.get(&code).map_or_else(|| AxisRange::fallback(code), |(_, r)| *r);
        self.last_level.insert(code, level);
        out.push(MacroEvent::new(elapsed, EV_ABS, code, range.level_value(level)));
    }

    fn update(&mut self, elapsed: Duration, code: u16) -> Vec<MacroEvent> {
        let mut out = Vec::new();
        match (self.cardinal, stick_partner(code)) {
            (true, Some((a, b))) => {
                let (pa, pb) = (self.position(a), self.position(b));
                let dom = match self.dominant.get(&a).copied() {
                    None => if pa.abs() >= pb.abs() { a } else { b },
                    Some(d) => {
                        let (pd, po, other) = if d == a { (pa, pb, b) } else { (pb, pa, a) };
                        if po.abs() > pd.abs() + DOMINANCE_MARGIN { other } else { d }
                    }
                };
                self.dominant.insert(a, dom);
                let other = if dom == a { b } else { a };
                // Release the other axis before moving the dominant one.
                self.set_level(&mut out, elapsed, other, 0);
                let level = self.target_level(dom);
                self.set_level(&mut out, elapsed, dom, level);
            }
            _ => {
                let level = self.target_level(code);
                self.set_level(&mut out, elapsed, code, level);
            }
        }
        out
    }

    /// Events returning every deflected axis to rest, so a macro always ends neutral.
    fn finish(&mut self, elapsed: Duration, range_of: impl Fn(u16) -> AxisRange) -> Vec<MacroEvent> {
        let mut out = Vec::new();
        let mut codes: Vec<u16> = self.last_level.keys().copied().collect();
        codes.sort();
        for code in codes {
            if self.last_level[&code] != 0 {
                out.push(MacroEvent::new(elapsed, EV_ABS, code, range_of(code).rest()));
                self.last_level.insert(code, 0);
            }
        }
        out
    }
}

type EchoMap = HashMap<(PathBuf, u16, u16, i32), (usize, Instant)>;

/// Injects one event (+ SYN) into `device_path`, registering it for echo cancellation.
fn inject(
    write_devices: &mut HashMap<PathBuf, Device>,
    ignore_injected: &mut EchoMap,
    device_path: &PathBuf,
    t: u16,
    c: u16,
    v: i32,
) -> bool {
    if !write_devices.contains_key(device_path) {
        match Device::open(device_path) {
            Ok(d) => { write_devices.insert(device_path.clone(), d); }
            Err(e) => {
                error!("Cannot open {:?} for injection: {}", device_path, e);
                return false;
            }
        }
    }
    let injection = InputEvent::new(evdev::EventType(t), c, v);
    let sync = InputEvent::new(evdev::EventType::SYNCHRONIZATION, 0, 0);
    let mut sent = write_devices.get_mut(device_path).unwrap().send_events(&[injection, sync]).is_ok();
    if !sent {
        // The cached handle may point to a device that was removed and
        // re-created at the same path (Steam recreates its virtual gamepad).
        write_devices.remove(device_path);
        if let Ok(mut d) = Device::open(device_path) {
            sent = d.send_events(&[injection, sync]).is_ok();
            write_devices.insert(device_path.clone(), d);
        }
    }
    if sent {
        let e = ignore_injected.entry((device_path.clone(), t, c, v)).or_insert((0, Instant::now()));
        e.0 += 1;
        e.1 = Instant::now();
        true
    } else {
        error!("Injection into {:?} failed", device_path);
        false
    }
}

// ---- Steam Deck back buttons (L4/L5/R4/R5) ----
// Steam's virtual gamepad has no back buttons, and hid-steam stops its own
// evdev device while Steam runs. So they are read from the Deck controller's
// raw HID reports (hidraw), alongside Steam: every hidraw reader gets its own
// copy of each report, nothing is modified. Report layout (hid-steam.c):
// 64 bytes, data[0]=1, data[1]=0, data[2]=9 (ID_CONTROLLER_DECK_STATE).
const VALVE_VID: &str = "000028DE";
const STEAM_DECK_PID: &str = "00001205";
const DECK_PADDLES: [(u16, usize, u8); 4] = [
    (0x224, 13, 1), // L4  BTN_GRIPL
    (0x225, 13, 2), // R4  BTN_GRIPR
    (0x226, 9, 7),  // L5  BTN_GRIPL2
    (0x227, 10, 0), // R5  BTN_GRIPR2
];
/// Pseudo device path used for back-button events read from hidraw.
const DECK_PADDLES_PATH: &str = "hidraw:steam-deck-back-buttons";

fn is_paddle_code(code: u16) -> bool {
    DECK_PADDLES.iter().any(|(c, _, _)| *c == code)
}

/// Returns the paddle state (code, pressed) changes between two reports.
fn paddle_changes(report: &[u8], last: &mut [bool; 4]) -> Vec<(u16, bool)> {
    let mut out = Vec::new();
    if report.len() != 64 || report[0] != 1 || report[1] != 0 || report[2] != 9 {
        return out;
    }
    for (i, (code, byte, bit)) in DECK_PADDLES.iter().enumerate() {
        let pressed = report[*byte] & (1 << bit) != 0;
        if pressed != last[i] {
            last[i] = pressed;
            out.push((*code, pressed));
        }
    }
    out
}

fn find_deck_hidraw() -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(dir) = fs::read_dir("/sys/class/hidraw") {
        for entry in dir.flatten() {
            let uevent = entry.path().join("device/uevent");
            if let Ok(text) = fs::read_to_string(&uevent) {
                let up = text.to_uppercase();
                if up.contains(&format!(":{}:{}", VALVE_VID, STEAM_DECK_PID)) {
                    found.push(PathBuf::from("/dev").join(entry.file_name()));
                }
            }
        }
    }
    found.sort();
    found
}

/// Starts one reader thread per Deck hidraw node; `on_change(code, pressed)`
/// is called on every back-button press/release. Returns the number of nodes.
fn spawn_paddle_readers<F>(on_change: F) -> usize
where
    F: Fn(u16, bool) + Send + Clone + 'static,
{
    let nodes = find_deck_hidraw();
    for node in &nodes {
        let node = node.clone();
        let cb = on_change.clone();
        thread::spawn(move || {
            use std::io::Read;
            let mut file = match fs::File::open(&node) {
                Ok(f) => f,
                Err(e) => {
                    warn!("Cannot open {:?} for back buttons: {}", node, e);
                    return;
                }
            };
            let mut buf = [0u8; 64];
            let mut last = [false; 4];
            loop {
                match file.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        for (code, pressed) in paddle_changes(&buf[..n], &mut last) {
                            cb(code, pressed);
                        }
                    }
                    _ => break,
                }
            }
        });
    }
    nodes.len()
}

// ---- Macro picker ----
// A third trigger opens an in-game list of all recorded macros (drawn by the
// overlay process, see overlay/picker.py). While it is open the daemon grabs
// the gamepad so the game doesn't see the navigation presses.

#[derive(Deserialize, Clone, Debug)]
struct PickerItem {
    name: String,
    path: String,
    #[serde(default)]
    loops: i32,
    #[serde(default = "default_true")]
    cancel_on_input: bool,
}

fn default_true() -> bool {
    true
}

/// Recorded macros listed in the picker file written by the plugin backend.
fn load_picker_items(path: &str) -> Vec<PickerItem> {
    let items: Vec<PickerItem> = fs::read_to_string(path)
        .ok()
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or_default();
    items
        .into_iter()
        .filter(|it| !load_macro(Path::new(&it.path)).is_empty())
        .collect()
}

#[derive(Debug, PartialEq)]
enum PickerAction {
    Up,
    Down,
    Launch,
    Close,
}

struct Picker {
    items: Vec<PickerItem>,
    selected: usize,
    dev: Device,
    target: PathBuf,
    stick_range: AxisRange,
    stick_latch: i32,
    last_input: Instant,
}

impl Picker {
    fn show(&self) {
        let names: Vec<&str> = self.items.iter().map(|i| i.name.as_str()).collect();
        let msg = serde_json::json!({ "title": "Choose a macro", "items": names, "selected": self.selected });
        println!("PICKER_SHOW {}", msg);
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }

    /// Maps a gamepad event to a picker action. D-pad (buttons or hat),
    /// left stick (with a latch so one push = one step), A launches, B closes.
    fn action_for(&mut self, ev: &InputEvent, picker_code: u16) -> Option<PickerAction> {
        match ev.kind() {
            InputEventKind::Key(k) if ev.value() == 1 => match k {
                Key::BTN_DPAD_UP => Some(PickerAction::Up),
                Key::BTN_DPAD_DOWN => Some(PickerAction::Down),
                Key::BTN_SOUTH => Some(PickerAction::Launch),
                Key::BTN_EAST => Some(PickerAction::Close),
                k if k.code() == picker_code => Some(PickerAction::Close),
                _ => None,
            },
            InputEventKind::AbsAxis(a) if a.0 == ABS_HAT0Y => match ev.value() {
                v if v < 0 => Some(PickerAction::Up),
                v if v > 0 => Some(PickerAction::Down),
                _ => None,
            },
            InputEventKind::AbsAxis(a) if a.0 == ABS_Y => {
                let p = self.stick_range.level_position(ev.value()) / ANALOG_LEVELS as f32;
                if p.abs() < 0.3 {
                    self.stick_latch = 0;
                    None
                } else if p <= -0.6 && self.stick_latch != -1 {
                    self.stick_latch = -1;
                    Some(PickerAction::Up)
                } else if p >= 0.6 && self.stick_latch != 1 {
                    self.stick_latch = 1;
                    Some(PickerAction::Down)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn step(&mut self, delta: i32) {
        let n = self.items.len() as i32;
        if n > 0 {
            self.selected = (((self.selected as i32 + delta) % n + n) % n) as usize;
        }
    }

    fn close(mut self) {
        let _ = self.dev.ungrab();
        println!("PICKER_HIDE");
        use std::io::Write;
        let _ = std::io::stdout().flush();
    }
}

/// Opens `target` for exclusive, non-blocking reading.
fn open_grabbed(target: &Path) -> std::io::Result<Device> {
    use std::os::unix::io::AsRawFd;
    let mut dev = Device::open(target)?;
    let fd = dev.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    if let Err(e) = dev.grab() {
        warn!("Picker: could not grab {:?} ({}); the game will also see the menu presses", target, e);
    }
    Ok(dev)
}

/// Keys a trigger can be learnt from. Excludes A/B (used to navigate the Steam
/// UI), mouse buttons (trackpad clicks) and touch/digitizer events such as
/// BTN_TOUCH, which the touchscreen and trackpads send on every contact.
fn is_detectable_key(code: u16) -> bool {
    let a_b = code == Key::BTN_SOUTH.code() || code == Key::BTN_EAST.code();
    let mouse = (0x110..=0x117).contains(&code);
    let digitizer = (0x140..=0x14f).contains(&code);
    !(a_b || mouse || digitizer)
}

/// `--detect-button <ms>`: prints the first key pressed on any input device
/// ("DETECTED <code> <name> <device>") or "DETECT_TIMEOUT", then exits.
/// A and B are ignored: they are used to navigate the Steam UI.
fn detect_button(timeout_ms: u64) -> Result<()> {
    let (tx, rx) = mpsc::channel::<(u16, String)>();
    if let Ok(dir) = fs::read_dir("/dev/input") {
        for entry in dir.flatten() {
            let path = entry.path();
            if !path.to_string_lossy().contains("event") {
                continue;
            }
            if let Ok(mut device) = Device::open(&path) {
                // Touchscreens / touchpads (multi-touch) can't be triggers.
                let is_touch = device
                    .supported_absolute_axes()
                    .map_or(false, |a| a.contains(evdev::AbsoluteAxisType::ABS_MT_POSITION_X));
                if is_touch {
                    continue;
                }
                let name = device.name().unwrap_or("?").to_string();
                let tx = tx.clone();
                thread::spawn(move || {
                    while let Ok(events) = device.fetch_events() {
                        for ev in events {
                            if let InputEventKind::Key(k) = ev.kind() {
                                if ev.value() == 1 && is_detectable_key(k.code()) {
                                    let _ = tx.send((k.code(), name.clone()));
                                }
                            }
                        }
                    }
                });
            }
        }
    }
    let tx_paddles = tx.clone();
    spawn_paddle_readers(move |code, pressed| {
        if pressed {
            let _ = tx_paddles.send((code, "Steam Deck back buttons (hidraw)".to_string()));
        }
    });
    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok((code, device)) => {
            let label = if is_paddle_code(code) {
                key_label(code).split(' ').next().unwrap_or("?").to_string()
            } else {
                format!("{:?}", Key::new(code))
            };
            println!("DETECTED {} {} {}", code, label, device);
            Ok(())
        }
        Err(_) => {
            println!("DETECT_TIMEOUT");
            std::process::exit(2);
        }
    }
}

enum Msg {
    /// A device was opened.
    Device {
        path: PathBuf,
        name: String,
        is_gamepad: bool,
        /// Ranges of its analog axes (gamepads only).
        ranges: Vec<(u16, AxisRange)>,
        has_record_key: bool,
        has_replay_key: bool,
    },
    Input(PathBuf, InputEvent),
    /// A device disappeared (its reader stopped).
    Removed(PathBuf),
}

#[derive(PartialEq)]
enum State {
    Idle,
    Recording,
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    info!("Starting Decky Macro v{} (analog support)...", env!("CARGO_PKG_VERSION"));

    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--detect-button") {
        let ms = args.get(pos + 1).and_then(|v| v.parse().ok()).unwrap_or(10000);
        return detect_button(ms);
    }
    let mut config_path = String::new();
    let mut macro_file = String::new();
    let mut loops: i32 = 0;
    let mut cancel_on_input = true;
    let mut picker_file = String::new();
    let mut record_analog = true;
    let mut cardinal_only = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => { config_path = args[i + 1].clone(); i += 2; }
            "--macro-file" => { macro_file = args[i + 1].clone(); i += 2; }
            "--loops" => { loops = args[i + 1].parse().unwrap_or(0); i += 2; }
            "--cancel-on-input" => { cancel_on_input = args[i + 1] == "true"; i += 2; }
            "--record-analog" => { record_analog = args[i + 1] == "true"; i += 2; }
            "--cardinal-only" => { cardinal_only = args[i + 1] == "true"; i += 2; }
            "--picker-file" => { picker_file = args[i + 1].clone(); i += 2; }
            _ => i += 1,
        }
    }

    let mut config = load_or_create_config(&config_path);
    let active_macro_path = PathBuf::from(&macro_file);
    let mut recorded_events = load_macro(&active_macro_path);

    let reload_config = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&reload_config))?;

    info!("Loaded active slot: {:?}", active_macro_path);
    info!("Loops: {}, Cancel: {}, Record analog: {}, 4 directions: {}", loops, cancel_on_input, record_analog, cardinal_only);
    info!("Triggers: record={}, replay={}", key_label(config.trigger_record_code), key_label(config.trigger_replay_code));
    for (name, t, mode, count) in [
        ("record", Trigger::Record, config.record_trigger_mode, config.record_press_count),
        ("replay", Trigger::Replay, config.replay_trigger_mode, config.replay_press_count),
        ("picker", Trigger::Picker, config.picker_trigger_mode, config.picker_press_count),
    ] {
        match mode {
            TriggerMode::Hold => info!("Trigger {}: long press {} ms", name, config.hold(t).as_millis()),
            TriggerMode::MultiPress => info!(
                "Trigger {}: {} presses, max {} ms apart",
                name, count, config.press_window(t).as_millis()
            ),
        }
    }

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_discovery = tx.clone();
    let (record_code, replay_code) = (config.trigger_record_code, config.trigger_replay_code);

    let tx_paddles = tx.clone();
    let paddle_path = PathBuf::from(DECK_PADDLES_PATH);
    let paddle_nodes = spawn_paddle_readers(move |code, pressed| {
        let ev = InputEvent::new(evdev::EventType::KEY, code, pressed as i32);
        let _ = tx_paddles.send(Msg::Input(paddle_path.clone(), ev));
    });
    if paddle_nodes > 0 {
        info!("Reading Steam Deck back buttons from {} hidraw node(s)", paddle_nodes);
        let _ = tx.send(Msg::Device {
            path: PathBuf::from(DECK_PADDLES_PATH),
            name: "Steam Deck back buttons".to_string(),
            is_gamepad: false,
            ranges: Vec::new(),
            has_record_key: is_paddle_code(record_code),
            has_replay_key: is_paddle_code(replay_code),
        });
    }

    // Paths with a live reader. A reader removes its path when its device goes
    // away, so a new device at the same path (Steam recreating its virtual
    // gamepad when a game starts, after a reboot...) is picked up again.
    let opened_paths: Arc<std::sync::Mutex<HashSet<PathBuf>>> = Arc::new(std::sync::Mutex::new(HashSet::new()));
    thread::spawn(move || {
        loop {
            if let Ok(input_dir) = std::fs::read_dir("/dev/input") {
                for entry in input_dir.flatten() {
                    let path = entry.path();
                    if path.to_string_lossy().contains("event") {
                        if !opened_paths.lock().unwrap().contains(&path) {
                            if let Ok(mut device) = Device::open(&path) {
                                opened_paths.lock().unwrap().insert(path.clone());

                                // Analog axes are only taken from gamepads. Motion
                                // sensors and touchscreens also report ABS_X/ABS_Y
                                // and must not leak into macros.
                                let is_gamepad = device
                                    .supported_keys()
                                    .map_or(false, |k| k.contains(Key::BTN_SOUTH));
                                let mut analog_codes: HashSet<u16> = HashSet::new();
                                let mut ranges = Vec::new();
                                if is_gamepad {
                                    if let (Some(axes), Ok(state)) =
                                        (device.supported_absolute_axes(), device.get_abs_state())
                                    {
                                        for code in ANALOG_AXES {
                                            if axes.contains(evdev::AbsoluteAxisType(code)) {
                                                let info = state[code as usize];
                                                let range = if info.maximum > info.minimum {
                                                    AxisRange { min: info.minimum, max: info.maximum }
                                                } else {
                                                    AxisRange::fallback(code)
                                                };
                                                analog_codes.insert(code);
                                                ranges.push((code, range));
                                            }
                                        }
                                    }
                                    info!(
                                        "Gamepad {:?} ({}): analog axes {:?}",
                                        path,
                                        device.name().unwrap_or("?"),
                                        ranges
                                    );
                                }
                                let keys = device.supported_keys();
                                let has_record_key = keys.map_or(false, |k| k.contains(Key::new(record_code)));
                                let has_replay_key = keys.map_or(false, |k| k.contains(Key::new(replay_code)));
                                if has_record_key || has_replay_key {
                                    info!(
                                        "{:?} ({}) reports trigger key(s):{}{}",
                                        path,
                                        device.name().unwrap_or("?"),
                                        if has_record_key { " record" } else { "" },
                                        if has_replay_key { " replay" } else { "" }
                                    );
                                }
                                let _ = tx_discovery.send(Msg::Device {
                                    path: path.clone(),
                                    name: device.name().unwrap_or("?").to_string(),
                                    is_gamepad,
                                    ranges,
                                    has_record_key,
                                    has_replay_key,
                                });

                                let tx_inner = tx_discovery.clone();
                                let path_clone = path.clone();
                                let opened = Arc::clone(&opened_paths);
                                thread::spawn(move || {
                                    loop {
                                        match device.fetch_events() {
                                            Ok(events) => {
                                                for event in events {
                                                    let kind = event.kind();
                                                    let is_key = matches!(kind, InputEventKind::Key(_));
                                                    let is_dpad = matches!(
                                                        kind,
                                                        InputEventKind::AbsAxis(evdev::AbsoluteAxisType::ABS_HAT0X) |
                                                        InputEventKind::AbsAxis(evdev::AbsoluteAxisType::ABS_HAT0Y)
                                                    );
                                                    let is_analog = matches!(
                                                        kind,
                                                        InputEventKind::AbsAxis(a) if analog_codes.contains(&a.0)
                                                    );
                                                    if is_key || is_dpad || is_analog {
                                                        let _ = tx_inner.send(Msg::Input(path_clone.clone(), event));
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                info!("Input device {:?} gone ({})", path_clone, e);
                                                break;
                                            }
                                        }
                                    }
                                    opened.lock().unwrap().remove(&path_clone);
                                    let _ = tx_inner.send(Msg::Removed(path_clone));
                                });
                            }
                        }
                    }
                }
            }
            thread::sleep(Duration::from_secs(1));
        }
    });

    let mut state = State::Idle;
    let mut record_start_time = Instant::now();
    let mut is_replaying = false;
    let mut replay_index = 0;
    let mut replay_start_time = Instant::now();
    let mut current_iteration = 0;

    let mut record_held_since: Option<Instant> = None;
    let mut replay_held_since: Option<Instant> = None;
    let mut record_triggered_this_press = false;
    let mut replay_triggered_this_press = false;
    let mut record_presses = PressCounter::default();
    let mut replay_presses = PressCounter::default();
    let mut record_multi_fire = false;
    let mut replay_multi_fire = false;
    let mut picker_held_since: Option<Instant> = None;
    let mut picker_triggered_this_press = false;
    let mut picker_presses = PressCounter::default();
    let mut picker_multi_fire = false;
    let mut picker: Option<Picker> = None;
    // Slot macro/settings to restore once a macro started from the picker stops.
    let mut picker_restore: Option<(Vec<MacroEvent>, i32, bool)> = None;
    let mut force_replay = false;

    let mut write_devices: HashMap<PathBuf, Device> = HashMap::new();
    let mut ignore_injected: EchoMap = HashMap::new();
    let mut last_echo_purge = Instant::now();
    let mut gamepads: HashSet<PathBuf> = HashSet::new();
    // Most recently connected gamepad: playback target when no gamepad input
    // was seen yet (e.g. replay started with a back button right after boot).
    let mut last_gamepad: Option<PathBuf> = None;
    let mut device_names: HashMap<PathBuf, String> = HashMap::new();
    let started_at = Instant::now();
    let mut trigger_check_done = false;
    let (mut seen_record_key, mut seen_replay_key) = (false, false);
    let mut last_active_device: Option<PathBuf> = None;

    // Analog bookkeeping.
    let mut axis_ranges: HashMap<(PathBuf, u16), AxisRange> = HashMap::new();
    let mut axis_state: HashMap<u16, (i32, AxisRange)> = HashMap::new(); // physical position
    let mut analog_recorder = AnalogRecorder::new(cardinal_only);
    // Last value injected by the current playback for every (type, code):
    // used to keep sticks held and to release everything when playback stops.
    let mut injected_state: HashMap<(u16, u16), i32> = HashMap::new();
    // Set when playback stops (finish/cancel) so buttons/sticks are released.
    let mut pending_recenter = false;
    // Per-playback statistics, logged when it stops.
    let mut axis_overrides: u64 = 0;
    let mut injected_count: u64 = 0;
    let mut playback_started = Instant::now();

    let range_for = |ranges: &HashMap<(PathBuf, u16), AxisRange>, dev: &Option<PathBuf>, code: u16| -> AxisRange {
        dev.as_ref()
            .and_then(|d| ranges.get(&(d.clone(), code)).copied())
            .unwrap_or_else(|| AxisRange::fallback(code))
    };

    loop {
        if reload_config.swap(false, Ordering::Relaxed) {
            info!("SIGHUP received, reloading global config from {:?}", config_path);
            config = load_or_create_config(&config_path);
            info!("Reloaded config: record={}, replay={}, hold={}ms",
                config.trigger_record_code, config.trigger_replay_code, config.hold_duration_ms);
        }

        let timeout = if is_replaying && replay_index < recorded_events.len() {
            let next_event = &recorded_events[replay_index];
            let target_time = replay_start_time + Duration::from_millis(next_event.delay_ms);
            let now = Instant::now();
            if target_time > now { Some(target_time - now) } else { Some(Duration::from_secs(0)) }
        } else {
            Some(Duration::from_millis(50))
        };
        let timeout = if picker.is_some() { timeout.map(|t| t.min(Duration::from_millis(16))) } else { timeout };

        let msg = if let Some(t) = timeout { rx.recv_timeout(t).ok() } else { rx.recv().ok() };
        let now = Instant::now();

        // Picker navigation, read from the grabbed gamepad.
        if let Some(p) = picker.as_mut() {
            let mut actions = Vec::new();
            let mut gone = false;
            let fetched = p.dev.fetch_events().map(|evs| evs.collect::<Vec<InputEvent>>());
            match fetched {
                Ok(evs) => {
                    for ev in evs {
                        if let Some(a) = p.action_for(&ev, config.trigger_picker_code) {
                            actions.push(a);
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => gone = true,
            }
            let mut launch: Option<PickerItem> = None;
            let mut close = gone || now.duration_since(p.last_input) > Duration::from_secs(20);
            for a in actions {
                p.last_input = now;
                match a {
                    PickerAction::Up => { p.step(-1); p.show(); }
                    PickerAction::Down => { p.step(1); p.show(); }
                    PickerAction::Launch => { launch = p.items.get(p.selected).cloned(); close = true; }
                    PickerAction::Close => close = true,
                }
                if close { break; }
            }
            if close {
                let p = picker.take().unwrap();
                let target = p.target.clone();
                p.close();
                if let Some(item) = launch {
                    let events = load_macro(Path::new(&item.path));
                    info!("Picker: playing \"{}\" ({} events)", item.name, events.len());
                    let saved = std::mem::replace(&mut recorded_events, events);
                    picker_restore = Some((saved, loops, cancel_on_input));
                    loops = item.loops;
                    cancel_on_input = item.cancel_on_input;
                    last_active_device = Some(target);
                    force_replay = true;
                }
            }
        }

        if now.duration_since(last_echo_purge) >= Duration::from_secs(1) {
            ignore_injected.retain(|_, (count, at)| *count > 0 && now.duration_since(*at) < ECHO_TTL);
            last_echo_purge = now;
        }

        // Warn once if the chosen trigger buttons are not reported by any device
        // (e.g. back buttons hidden behind Steam's virtual gamepad).
        if !trigger_check_done && now.duration_since(started_at) >= Duration::from_secs(3) {
            trigger_check_done = true;
            if !seen_record_key {
                error!("Trigger not available: record button {} is not reported by any input device.", key_label(config.trigger_record_code));
            }
            if !seen_replay_key {
                error!("Trigger not available: replay button {} is not reported by any input device.", key_label(config.trigger_replay_code));
            }
        }

        let input = match msg {
            Some(Msg::Device { path, name, is_gamepad, ranges, has_record_key, has_replay_key }) => {
                for (code, range) in ranges {
                    axis_ranges.insert((path.clone(), code), range);
                }
                device_names.insert(path.clone(), name);
                // A device re-created at a known path: drop the stale write handle.
                write_devices.remove(&path);
                if is_gamepad {
                    gamepads.insert(path.clone());
                    last_gamepad = Some(path);
                }
                seen_record_key |= has_record_key;
                seen_replay_key |= has_replay_key;
                None
            }
            Some(Msg::Input(path, event)) => Some((path, event)),
            Some(Msg::Removed(path)) => {
                write_devices.remove(&path);
                gamepads.remove(&path);
                axis_ranges.retain(|(p, _), _| p != &path);
                if last_gamepad.as_ref() == Some(&path) {
                    last_gamepad = gamepads.iter().next().cloned();
                }
                if last_active_device.as_ref() == Some(&path) && !is_replaying {
                    last_active_device = None;
                }
                None
            }
            None => None,
        };

        if let Some((device_path, event)) = input {
            let kind = event.kind();
            let value = event.value();

            let key_tuple = (device_path.clone(), event.event_type().0, event.code(), value);
            if let Some((count, at)) = ignore_injected.get_mut(&key_tuple) {
                if *count > 0 && now.duration_since(*at) < ECHO_TTL {
                    *count -= 1;
                    continue;
                }
                // Stale: the echo never came. Treat this event as physical input.
                ignore_injected.remove(&key_tuple);
            }

            let analog = match kind {
                InputEventKind::AbsAxis(a) if is_analog_axis(a.0) => {
                    let range = axis_ranges
                        .get(&(device_path.clone(), a.0))
                        .copied()
                        .unwrap_or_else(|| AxisRange::fallback(a.0));
                    axis_state.insert(a.0, (value, range));
                    Some((a.0, range))
                }
                _ => None,
            };

            // Small stick drift must not redirect playback to another device.
            let is_significant = match analog {
                Some((_, range)) => range.deflection(value) >= ANALOG_DEADZONE,
                None => true,
            };
            let is_record_btn = matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_record_code);
            let is_replay_btn = matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_replay_code);

            // Playback goes to the gamepad used last. The target stays fixed while
            // a macro plays, and trigger buttons / non-gamepad devices (keyboard,
            // Steam's virtual keyboard...) never become the target.
            let is_gamepad_dev = gamepads.is_empty() || gamepads.contains(&device_path);
            if is_significant && !is_replaying && is_gamepad_dev && !is_record_btn && !is_replay_btn
                && !(config.trigger_picker_code != 0 && matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_picker_code))
            {
                last_active_device = Some(device_path.clone());
            }

            let is_picker_btn = config.trigger_picker_code != 0
                && matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_picker_code);
            if is_picker_btn {
                if value == 1 {
                    if picker_held_since.is_none() {
                        picker_held_since = Some(now);
                        picker_triggered_this_press = false;
                        if config.picker_trigger_mode == TriggerMode::MultiPress
                            && picker_presses.press(now, config.press_window(Trigger::Picker), config.picker_press_count)
                        {
                            picker_multi_fire = true;
                        }
                    }
                } else if value == 0 {
                    picker_held_since = None;
                }
            }

            if is_record_btn {
                if value == 1 {
                    // Only act on a fresh press edge; ignore duplicate/echoed
                    // value==1 events so a 0ms hold cannot trigger twice.
                    if record_held_since.is_none() {
                        record_held_since = Some(now);
                        record_triggered_this_press = false;
                        if config.record_trigger_mode == TriggerMode::MultiPress
                            && record_presses.press(now, config.press_window(Trigger::Record), config.record_press_count)
                        {
                            record_multi_fire = true;
                        }
                    }
                } else if value == 0 {
                    record_held_since = None;
                }
            }
            if is_replay_btn {
                if value == 1 {
                    if replay_held_since.is_none() {
                        replay_held_since = Some(now);
                        replay_triggered_this_press = false;
                        if config.replay_trigger_mode == TriggerMode::MultiPress
                            && replay_presses.press(now, config.press_window(Trigger::Replay), config.replay_press_count)
                        {
                            replay_multi_fire = true;
                        }
                    }
                } else if value == 0 {
                    replay_held_since = None;
                }
            }

            let is_deliberate = match analog {
                Some((_, range)) => range.deflection(value) > ANALOG_CANCEL_THRESHOLD,
                None => value != 0,
            };
            let is_system = matches!(kind, InputEventKind::Key(k) if is_system_key(k.code()));
            if is_replaying && is_deliberate && !is_system && !is_record_btn && !is_replay_btn && !is_picker_btn && cancel_on_input {
                let lag_ms = std::time::SystemTime::now()
                    .duration_since(event.timestamp())
                    .ok()
                    .filter(|d| d.as_secs() < 3600) // synthetic events have no timestamp
                    .map(|d| d.as_millis())
                    .unwrap_or(0);
                warn!("Playback cancelled by physical input: {:?} (seen {} ms after the press)", kind, lag_ms);
                is_replaying = false;
                pending_recenter = true;
            }

            // Steam keeps rewriting the real stick position into its virtual
            // gamepad, which instantly undoes an injected stick value. While the
            // macro holds an axis, put the macro value back whenever something
            // else overwrites it (unless it was a deliberate push, handled above).
            if is_replaying && !is_deliberate && last_active_device.as_ref() == Some(&device_path) {
                if let Some((code, _)) = analog {
                    if let Some(&held) = injected_state.get(&(EV_ABS, code)) {
                        if value != held {
                            if inject(&mut write_devices, &mut ignore_injected, &device_path, EV_ABS, code, held) {
                                axis_overrides += 1;
                            }
                        }
                    }
                }
            }

            // Back buttons can trigger and cancel, but can't be recorded: the
            // virtual gamepad the game reads has no such buttons to replay.
            let from_paddles = device_path.as_os_str() == DECK_PADDLES_PATH;
            if state == State::Recording && !is_record_btn && !is_replay_btn && !is_picker_btn && !from_paddles && !is_system {
                if recorded_events.len() >= MAX_EVENTS {
                    state = State::Idle;
                    info!("Macro size limit reached. Stopped recording.");
                    let elapsed = record_start_time.elapsed();
                    let tail = analog_recorder.finish(elapsed, |c| range_for(&axis_ranges, &last_active_device, c));
                    recorded_events.extend(tail);
                    save_macro(&active_macro_path, &recorded_events);
                } else if let Some((code, range)) = analog {
                    if record_analog {
                        recorded_events.extend(analog_recorder.record(record_start_time.elapsed(), code, value, range));
                    }
                } else {
                    recorded_events.push(MacroEvent::from_evdev(record_start_time.elapsed(), event));
                }
            }
        }

        let record_fire = match config.record_trigger_mode {
            TriggerMode::MultiPress => std::mem::take(&mut record_multi_fire),
            TriggerMode::Hold => {
                let fire = record_held_since.map_or(false, |held| {
                    !record_triggered_this_press
                        && held.elapsed() >= config.hold(Trigger::Record)
                        && replay_held_since.is_none()
                });
                if fire {
                    record_triggered_this_press = true;
                }
                fire
            }
        };
        if record_fire && picker.is_none() {
            if state == State::Idle {
                state = State::Recording;
                if let Some((ev, l, c)) = picker_restore.take() {
                    recorded_events = ev;
                    loops = l;
                    cancel_on_input = c;
                }
                recorded_events.clear();
                record_start_time = Instant::now();
                if is_replaying {
                    pending_recenter = true;
                }
                is_replaying = false;
                if record_analog {
                    recorded_events.extend(analog_recorder.start(&axis_state));
                }
                info!("Started recording to {:?}", active_macro_path);
            } else {
                state = State::Idle;
                let has_input = !recorded_events.is_empty();
                if !has_input {
                    error!("Recording stopped with no inputs; previous macro kept.");
                    // Keep in-memory state consistent with the (unchanged)
                    // file so a later replay uses the previous macro, if any.
                    recorded_events = load_macro(&active_macro_path);
                } else {
                    let elapsed = record_start_time.elapsed();
                    let tail = analog_recorder.finish(elapsed, |c| range_for(&axis_ranges, &last_active_device, c));
                    recorded_events.extend(tail);
                    save_macro(&active_macro_path, &recorded_events);
                    info!("Stopped recording. Saved {} events.", recorded_events.len());
                }
            }
        }

        let replay_fire = match config.replay_trigger_mode {
            TriggerMode::MultiPress => std::mem::take(&mut replay_multi_fire),
            TriggerMode::Hold => {
                let fire = replay_held_since.map_or(false, |held| {
                    !replay_triggered_this_press
                        && held.elapsed() >= config.hold(Trigger::Replay)
                        && record_held_since.is_none()
                });
                if fire {
                    replay_triggered_this_press = true;
                }
                fire
            }
        };
        // Picker trigger: open the list (stopping any playback), or close it.
        let picker_fire = config.trigger_picker_code != 0 && match config.picker_trigger_mode {
            TriggerMode::MultiPress => std::mem::take(&mut picker_multi_fire),
            TriggerMode::Hold => {
                let fire = picker_held_since.map_or(false, |held| {
                    !picker_triggered_this_press && held.elapsed() >= config.hold(Trigger::Picker)
                });
                if fire {
                    picker_triggered_this_press = true;
                }
                fire
            }
        };
        if picker_fire && state == State::Idle {
            if let Some(p) = picker.take() {
                p.close();
            } else {
                if is_replaying {
                    is_replaying = false;
                    pending_recenter = true;
                }
                let items = load_picker_items(&picker_file);
                let target_ok = last_active_device.as_ref().map_or(false, |d| gamepads.contains(d));
                let target = if target_ok { last_active_device.clone() } else { last_gamepad.clone() };
                if items.is_empty() {
                    error!("Picker: no recorded macro to choose from");
                } else if let Some(target) = target {
                    match open_grabbed(&target) {
                        Ok(dev) => {
                            let stick_range = axis_ranges
                                .get(&(target.clone(), ABS_Y))
                                .copied()
                                .unwrap_or_else(|| AxisRange::fallback(ABS_Y));
                            let p = Picker { items, selected: 0, dev, target, stick_range, stick_latch: 0, last_input: now };
                            info!("Picker opened with {} macro(s)", p.items.len());
                            p.show();
                            picker = Some(p);
                        }
                        Err(e) => error!("Picker: cannot open gamepad {:?}: {}", target, e),
                    }
                } else {
                    error!("Playback failed: no gamepad found to send the macro to");
                }
            }
        }

        if (replay_fire && picker.is_none()) || std::mem::take(&mut force_replay) {
            if state == State::Idle {
                // Target: the gamepad used last, else the newest gamepad.
                let target_ok = last_active_device.as_ref().map_or(false, |d| gamepads.is_empty() || gamepads.contains(d));
                if !target_ok {
                    last_active_device = last_gamepad.clone();
                }
                if recorded_events.is_empty() {
                    error!("Playback failed: no recorded inputs for {:?}", active_macro_path);
                } else if last_active_device.is_none() {
                    error!("Playback failed: no gamepad found to send the macro to");
                } else {
                    let target = last_active_device.clone().unwrap();
                    info!(
                        "Playback target: {:?} ({}), {} events",
                        target,
                        device_names.get(&target).map(String::as_str).unwrap_or("?"),
                        recorded_events.len()
                    );
                    is_replaying = true;
                    replay_index = 0;
                    current_iteration = 0;
                    replay_start_time = Instant::now();
                    axis_overrides = 0;
                    injected_count = 0;
                    playback_started = Instant::now();
                    info!("Started playback from {:?}", active_macro_path);
                }
            }
        }

        if is_replaying && replay_index < recorded_events.len() {
            let next_event = &recorded_events[replay_index];
            if Instant::now() >= replay_start_time + Duration::from_millis(next_event.delay_ms) {
                if let Some(device_path) = last_active_device.clone() {
                    let (t, c, v) = (next_event.event_type, next_event.code, next_event.value);
                    if inject(&mut write_devices, &mut ignore_injected, &device_path, t, c, v) {
                        injected_count += 1;
                        injected_state.insert((t, c), v);
                    }
                }
                replay_index += 1;
            }
        } else if is_replaying && replay_index >= recorded_events.len() {
            if loops <= 0 || current_iteration < loops - 1 {
                current_iteration += 1;
                replay_index = 0;
                replay_start_time = Instant::now();
                info!("Looping macro (Iteration {})", current_iteration);
            } else {
                is_replaying = false;
                pending_recenter = true;
                info!("Playback finished.");
            }
        }

        // Never leave a button, stick or trigger held after playback ends or is
        // cancelled (a loop is usually cancelled in the middle of a press).
        if pending_recenter && !is_replaying {
            pending_recenter = false;
            let cleanup_start = Instant::now();
            let mut released = 0;
            if let Some(device_path) = last_active_device.clone() {
                let mut held: Vec<((u16, u16), i32)> = injected_state.drain().collect();
                held.sort();
                for ((t, c), last) in held {
                    let rest = if t == EV_ABS && is_analog_axis(c) {
                        range_for(&axis_ranges, &last_active_device, c).rest()
                    } else {
                        0
                    };
                    if last != rest && inject(&mut write_devices, &mut ignore_injected, &device_path, t, c, rest) {
                        released += 1;
                    }
                }
            }
            injected_state.clear();
            info!(
                "Playback stats: {} iteration(s), {} events injected, {} stick re-asserts, {:.1}s; released {} held input(s) in {} ms.",
                current_iteration + 1,
                injected_count,
                axis_overrides,
                playback_started.elapsed().as_secs_f32(),
                released,
                cleanup_start.elapsed().as_millis()
            );
            axis_overrides = 0;
        }

        // Back to the selected slot once a macro started from the picker ends.
        if !is_replaying && !pending_recenter {
            if let Some((ev, l, c)) = picker_restore.take() {
                recorded_events = ev;
                loops = l;
                cancel_on_input = c;
            }
        }

        // Detect a stalled main loop (would delay injections and cancellation).
        let busy = now.elapsed();
        if busy >= Duration::from_millis(50) {
            warn!("Main loop stalled for {} ms", busy.as_millis());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STICK: AxisRange = AxisRange { min: -32768, max: 32767 };
    const TRIGGER: AxisRange = AxisRange { min: 0, max: 255 };

    #[test]
    fn rest_positions() {
        assert_eq!(STICK.rest(), 0);
        assert_eq!(TRIGGER.rest(), 0);
        assert_eq!(AxisRange { min: 0, max: 1023 }.rest(), 0);
    }

    #[test]
    fn level_values() {
        assert_eq!(STICK.level_value(0), 0);
        assert_eq!(STICK.level_value(4), 32767);
        assert_eq!(STICK.level_value(-4), -32768);
        assert_eq!(STICK.level_value(2), 16383);
        assert_eq!(STICK.level_value(-1), -8192);
        assert_eq!(TRIGGER.level_value(1), 63);
        assert_eq!(TRIGGER.level_value(4), 255);
    }

    #[test]
    fn recorder_steps_with_hysteresis() {
        let mut r = AnalogRecorder::new(false);
        assert!(r.start(&HashMap::new()).is_empty());
        let t = Duration::from_millis(10);
        // drift (5%) and a light touch (15%): nothing
        assert!(r.record1(t, ABS_X, 1600, STICK).is_none());
        assert!(r.record1(t, ABS_X, 4900, STICK).is_none());
        // 20%: level 1 (25%)
        assert_eq!(r.record1(t, ABS_X, 6550, STICK).unwrap().value, 8191);
        // hovering around the 37.5% boundary: stays on level 1
        for v in [12000, 12700, 11800, 13500] {
            assert!(r.record1(t, ABS_X, v, STICK).is_none(), "{}", v);
        }
        // clearly at 50%: level 2
        assert_eq!(r.record1(t, ABS_X, 16400, STICK).unwrap().value, 16383);
        // full: level 4, exact max
        assert_eq!(r.record1(t, ABS_X, 32700, STICK).unwrap().value, 32767);
        // released to near centre: exact rest
        let ev = r.record1(t, ABS_X, -1000, STICK).unwrap();
        assert_eq!((ev.event_type, ev.code, ev.value), (EV_ABS, ABS_X, 0));
        // other direction
        assert_eq!(r.record1(t, ABS_X, -32768, STICK).unwrap().value, -32768);
    }

    #[test]
    fn full_sweep_is_compact() {
        let mut r = AnalogRecorder::new(false);
        r.start(&HashMap::new());
        let mut n = 0;
        for v in (-32768..=32767).step_by(16) {
            if r.record1(Duration::ZERO, ABS_RX, v, STICK).is_some() {
                n += 1;
            }
        }
        assert!(n == 9, "sweep produced {} events", n);
    }

    #[test]
    fn start_captures_held_axes_and_finish_recenters() {
        let mut r = AnalogRecorder::new(false);
        let mut cur = HashMap::new();
        cur.insert(ABS_Y, (-30000, STICK));
        cur.insert(ABS_X, (300, STICK)); // inside dead zone
        cur.insert(ABS_RZ, (255, TRIGGER));
        let init = r.start(&cur);
        assert_eq!(init.len(), 2);
        assert!(init.iter().all(|e| e.delay_ms == 0));
        assert!(init.contains(&MacroEvent { delay_ms: 0, event_type: EV_ABS, code: ABS_Y, value: -32768 }));
        assert!(init.contains(&MacroEvent { delay_ms: 0, event_type: EV_ABS, code: ABS_RZ, value: 255 }));

        let tail = r.finish(Duration::from_millis(1234), |c| AxisRange::fallback(c));
        assert_eq!(tail.len(), 2);
        assert!(tail.iter().all(|e| e.value == 0 && e.delay_ms == 1234));
        // second finish: nothing left to recentre
        assert!(r.finish(Duration::from_millis(1300), |c| AxisRange::fallback(c)).is_empty());
    }

    #[test]
    fn cancel_threshold() {
        assert!(STICK.deflection(5000) < ANALOG_CANCEL_THRESHOLD);
        assert!(STICK.deflection(-25000) > ANALOG_CANCEL_THRESHOLD);
        assert!(TRIGGER.deflection(200) > ANALOG_CANCEL_THRESHOLD);
    }

    #[test]
    fn macro_roundtrip_keeps_analog_events() {
        let dir = std::env::temp_dir().join("decky-macro-test");
        let path = dir.join("m.json");
        let ev = vec![
            MacroEvent { delay_ms: 0, event_type: EV_ABS, code: ABS_X, value: -32768 },
            MacroEvent { delay_ms: 15, event_type: 1, code: 304, value: 1 },
        ];
        save_macro(&path, &ev);
        assert_eq!(load_macro(&path), ev);
    }

    #[test]
    fn cardinal_keeps_dominant_axis_only() {
        let mut r = AnalogRecorder::new(true);
        r.start(&HashMap::new());
        let t = Duration::from_millis(5);
        // left with a bit of up: pure left
        let ev = r.record(t, ABS_X, -32768, STICK);
        assert_eq!(ev, vec![MacroEvent::new(t, EV_ABS, ABS_X, -32768)]);
        assert!(r.record(t, ABS_Y, -9000, STICK).is_empty(), "minor axis must stay at rest");
        // near-diagonal: dominance does not flip
        assert!(r.record(t, ABS_Y, -30000, STICK).is_empty());
        // clearly up now (X back to ~25%): switch to pure up, X released first
        let ev = r.record(t, ABS_X, -8000, STICK);
        assert_eq!(ev, vec![
            MacroEvent::new(t, EV_ABS, ABS_X, 0),
            MacroEvent::new(t, EV_ABS, ABS_Y, -32768),
        ]);
        // sticks are independent; triggers unaffected
        assert_eq!(r.record(t, ABS_RX, 32767, STICK), vec![MacroEvent::new(t, EV_ABS, ABS_RX, 32767)]);
        assert_eq!(r.record(t, ABS_RZ, 255, TRIGGER), vec![MacroEvent::new(t, EV_ABS, ABS_RZ, 255)]);
        // finish recentres everything still deflected
        let tail = r.finish(Duration::from_millis(9), |c| AxisRange::fallback(c));
        assert_eq!(tail.len(), 3);
        assert!(tail.iter().all(|e| e.value == 0));
    }

    #[test]
    fn cardinal_start_snapshot() {
        let mut r = AnalogRecorder::new(true);
        let mut cur = HashMap::new();
        cur.insert(ABS_X, (20000, STICK));
        cur.insert(ABS_Y, (-25000, STICK));
        let init = r.start(&cur);
        assert_eq!(init, vec![MacroEvent::new(Duration::ZERO, EV_ABS, ABS_Y, -24576)]);
    }

    #[test]
    fn detection_ignores_touch_mouse_and_ui_buttons() {
        assert!(!is_detectable_key(Key::BTN_TOUCH.code()));
        assert!(!is_detectable_key(Key::BTN_TOOL_FINGER.code()));
        assert!(!is_detectable_key(Key::BTN_LEFT.code()));
        assert!(!is_detectable_key(Key::BTN_SOUTH.code()));
        assert!(!is_detectable_key(Key::BTN_EAST.code()));
        assert!(is_detectable_key(Key::KEY_F10.code()));
        assert!(is_detectable_key(Key::BTN_THUMBR.code()));
        assert!(is_detectable_key(Key::BTN_TR.code()));
    }

    #[test]
    fn paddle_report_parsing() {
        let mut report = [0u8; 64];
        report[0] = 1;
        report[2] = 9;
        let mut last = [false; 4];
        assert!(paddle_changes(&report, &mut last).is_empty());
        report[10] |= 1; // R5
        assert_eq!(paddle_changes(&report, &mut last), vec![(0x227, true)]);
        assert!(paddle_changes(&report, &mut last).is_empty(), "no repeat while held");
        report[13] |= 0b10; // L4 too
        assert_eq!(paddle_changes(&report, &mut last), vec![(0x224, true)]);
        report[10] = 0;
        report[13] = 0;
        let mut ch = paddle_changes(&report, &mut last);
        ch.sort();
        assert_eq!(ch, vec![(0x224, false), (0x227, false)]);
        // other report types / malformed reports are ignored
        report[2] = 1;
        report[9] = 0x80;
        assert!(paddle_changes(&report, &mut last).is_empty());
        assert!(paddle_changes(&report[..32], &mut last).is_empty());
    }

    #[test]
    fn volume_keys_are_system_keys() {
        assert!(is_system_key(Key::KEY_VOLUMEUP.code()));
        assert!(is_system_key(Key::KEY_VOLUMEDOWN.code()));
        assert!(is_system_key(Key::KEY_MUTE.code()));
        assert!(is_system_key(Key::KEY_POWER.code()));
        assert!(!is_system_key(Key::BTN_SOUTH.code()));
        assert!(!is_system_key(Key::KEY_F10.code()));
    }

    #[test]
    fn multi_press_counter() {
        let w = Duration::from_millis(400);
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let mut c = PressCounter::default();
        assert!(!c.press(ms(0), w, 3));
        assert!(!c.press(ms(300), w, 3));
        assert!(c.press(ms(600), w, 3), "3rd press within the window fires");
        // series restarts after firing
        assert!(!c.press(ms(700), w, 3));
        // a pause longer than the window resets the count
        assert!(!c.press(ms(1200), w, 3));
        assert!(!c.press(ms(1500), w, 3));
        assert!(c.press(ms(1800), w, 3));
    }

    #[test]
    fn config_defaults_for_old_files() {
        let c: Config = serde_json::from_str(r#"{"trigger_record_code":317,"trigger_replay_code":318}"#).unwrap();
        assert_eq!(c.record_trigger_mode, TriggerMode::Hold);
        assert_eq!(c.replay_press_count, 3);
        assert_eq!(c.press_window_ms, 400);
        let c: Config = serde_json::from_str(
            r#"{"trigger_record_code":317,"trigger_replay_code":551,"replay_trigger_mode":"multi_press","replay_press_count":5}"#,
        ).unwrap();
        assert_eq!(c.replay_trigger_mode, TriggerMode::MultiPress);
        assert_eq!(c.replay_press_count, 5);
    }

    #[test]
    fn picker_items_skip_empty_macros() {
        let dir = std::env::temp_dir().join("decky-macro-picker-test");
        let _ = fs::create_dir_all(&dir);
        let full = dir.join("macro_1.json");
        let empty = dir.join("macro_2.json");
        save_macro(&full, &vec![MacroEvent { delay_ms: 0, event_type: 1, code: 304, value: 1 }]);
        save_macro(&empty, &vec![]);
        let list = dir.join("picker.json");
        fs::write(&list, serde_json::json!([
            {"name": "Farm", "path": full.to_string_lossy(), "loops": 0, "cancel_on_input": true},
            {"name": "Empty", "path": empty.to_string_lossy()},
            {"name": "Missing", "path": dir.join("nope.json").to_string_lossy()}
        ]).to_string()).unwrap();
        let items = load_picker_items(list.to_str().unwrap());
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Farm");
        assert!(load_picker_items("/nonexistent").is_empty());
    }

    #[test]
    fn per_trigger_timings_fall_back_to_shared_values() {
        let c: Config = serde_json::from_str(
            r#"{"trigger_record_code":317,"trigger_replay_code":318,"hold_duration_ms":700,"press_window_ms":300,
                "replay_hold_ms":0,"picker_press_window_ms":900}"#,
        ).unwrap();
        assert_eq!(c.hold(Trigger::Record), Duration::from_millis(700));
        assert_eq!(c.hold(Trigger::Replay), Duration::from_millis(0));
        assert_eq!(c.press_window(Trigger::Replay), Duration::from_millis(300));
        assert_eq!(c.press_window(Trigger::Picker), Duration::from_millis(900));
    }
}
