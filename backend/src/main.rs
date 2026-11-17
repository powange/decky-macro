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
struct AnalogRecorder {
    /// Last recorded level per axis (0 = rest).
    last_level: HashMap<u16, i32>,
}

impl AnalogRecorder {
    fn new() -> Self {
        Self { last_level: HashMap::new() }
    }

    /// Starts a recording. Axes already deflected (e.g. stick held while the
    /// record trigger fired) are written as initial events at t=0.
    fn start(&mut self, current: &HashMap<u16, (i32, AxisRange)>) -> Vec<MacroEvent> {
        self.last_level.clear();
        let mut initial = Vec::new();
        let mut codes: Vec<&u16> = current.keys().collect();
        codes.sort();
        for code in codes {
            let (value, range) = current[code];
            let level = range.level_from(value, 0);
            self.last_level.insert(*code, level);
            if level != 0 {
                initial.push(MacroEvent::new(Duration::ZERO, EV_ABS, *code, range.level_value(level)));
            }
        }
        initial
    }

    /// Returns the event to store, or None if the axis stayed on the same level.
    fn record(&mut self, elapsed: Duration, code: u16, value: i32, range: AxisRange) -> Option<MacroEvent> {
        let last = *self.last_level.get(&code).unwrap_or(&0);
        let level = range.level_from(value, last);
        if level == last {
            return None;
        }
        self.last_level.insert(code, level);
        Some(MacroEvent::new(elapsed, EV_ABS, code, range.level_value(level)))
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

/// Injects one event (+ SYN) into `device_path`, registering it for echo cancellation.
fn inject(
    write_devices: &mut HashMap<PathBuf, Device>,
    ignore_injected: &mut HashMap<(PathBuf, u16, u16, i32), usize>,
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
    let dev = write_devices.get_mut(device_path).unwrap();
    let injection = InputEvent::new(evdev::EventType(t), c, v);
    let sync = InputEvent::new(evdev::EventType::SYNCHRONIZATION, 0, 0);
    if dev.send_events(&[injection, sync]).is_ok() {
        *ignore_injected.entry((device_path.clone(), t, c, v)).or_insert(0) += 1;
        true
    } else {
        false
    }
}

enum Msg {
    /// A gamepad was opened; carries the ranges of its analog axes.
    Gamepad(PathBuf, Vec<(u16, AxisRange)>),
    Input(PathBuf, InputEvent),
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
    let mut config_path = String::new();
    let mut macro_file = String::new();
    let mut loops: i32 = 0;
    let mut cancel_on_input = true;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--config" => { config_path = args[i + 1].clone(); i += 2; }
            "--macro-file" => { macro_file = args[i + 1].clone(); i += 2; }
            "--loops" => { loops = args[i + 1].parse().unwrap_or(0); i += 2; }
            "--cancel-on-input" => { cancel_on_input = args[i + 1] == "true"; i += 2; }
            _ => i += 1,
        }
    }

    let mut config = load_or_create_config(&config_path);
    let active_macro_path = PathBuf::from(&macro_file);
    let mut recorded_events = load_macro(&active_macro_path);

    let reload_config = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGHUP, Arc::clone(&reload_config))?;

    info!("Loaded active slot: {:?}", active_macro_path);
    info!("Loops: {}, Cancel: {}", loops, cancel_on_input);
    info!("Trigger hold duration: {} ms", config.hold_duration_ms);

    let (tx, rx) = mpsc::channel::<Msg>();
    let tx_discovery = tx.clone();

    thread::spawn(move || {
        let mut opened_paths = HashSet::new();
        loop {
            if let Ok(input_dir) = std::fs::read_dir("/dev/input") {
                for entry in input_dir.flatten() {
                    let path = entry.path();
                    if path.to_string_lossy().contains("event") {
                        if !opened_paths.contains(&path) {
                            if let Ok(mut device) = Device::open(&path) {
                                opened_paths.insert(path.clone());

                                // Analog axes are only taken from gamepads. Motion
                                // sensors and touchscreens also report ABS_X/ABS_Y
                                // and must not leak into macros.
                                let is_gamepad = device
                                    .supported_keys()
                                    .map_or(false, |k| k.contains(Key::BTN_SOUTH));
                                let mut analog_codes: HashSet<u16> = HashSet::new();
                                if is_gamepad {
                                    let mut ranges = Vec::new();
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
                                    let _ = tx_discovery.send(Msg::Gamepad(path.clone(), ranges));
                                }

                                let tx_inner = tx_discovery.clone();
                                let path_clone = path.clone();
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
                                            Err(_) => break,
                                        }
                                    }
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

    let mut write_devices: HashMap<PathBuf, Device> = HashMap::new();
    let mut ignore_injected: HashMap<(PathBuf, u16, u16, i32), usize> = HashMap::new();
    let mut last_active_device: Option<PathBuf> = None;

    // Analog bookkeeping.
    let mut axis_ranges: HashMap<(PathBuf, u16), AxisRange> = HashMap::new();
    let mut axis_state: HashMap<u16, (i32, AxisRange)> = HashMap::new(); // physical position
    let mut analog_recorder = AnalogRecorder::new();
    // Axes moved by the current playback, with their last injected value.
    let mut injected_axes: HashMap<u16, i32> = HashMap::new();
    // Set when playback stops (finish/cancel) so the sticks are recentred.
    let mut pending_recenter = false;
    // How many times a held axis had to be re-asserted during the current playback.
    let mut axis_overrides: u64 = 0;

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

        let msg = if let Some(t) = timeout { rx.recv_timeout(t).ok() } else { rx.recv().ok() };
        let now = Instant::now();

        let input = match msg {
            Some(Msg::Gamepad(path, ranges)) => {
                for (code, range) in ranges {
                    axis_ranges.insert((path.clone(), code), range);
                }
                None
            }
            Some(Msg::Input(path, event)) => Some((path, event)),
            None => None,
        };

        if let Some((device_path, event)) = input {
            let kind = event.kind();
            let value = event.value();

            let key_tuple = (device_path.clone(), event.event_type().0, event.code(), value);
            if let Some(count) = ignore_injected.get_mut(&key_tuple) {
                if *count > 0 {
                    *count -= 1;
                    continue;
                }
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
            // The playback target stays fixed while a macro is playing.
            if is_significant && !is_replaying {
                last_active_device = Some(device_path.clone());
            }

            let is_record_btn = matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_record_code);
            let is_replay_btn = matches!(kind, InputEventKind::Key(k) if k.code() == config.trigger_replay_code);

            if is_record_btn {
                if value == 1 {
                    // Only act on a fresh press edge; ignore duplicate/echoed
                    // value==1 events so a 0ms hold cannot trigger twice.
                    if record_held_since.is_none() {
                        record_held_since = Some(now);
                        record_triggered_this_press = false;
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
                    }
                } else if value == 0 {
                    replay_held_since = None;
                }
            }

            let is_deliberate = match analog {
                Some((_, range)) => range.deflection(value) > ANALOG_CANCEL_THRESHOLD,
                None => value != 0,
            };
            if is_replaying && is_deliberate && !is_record_btn && !is_replay_btn && cancel_on_input {
                warn!("Playback cancelled by physical input: {:?}", kind);
                is_replaying = false;
                pending_recenter = true;
            }

            // Steam keeps rewriting the real stick position into its virtual
            // gamepad, which instantly undoes an injected stick value. While the
            // macro holds an axis, put the macro value back whenever something
            // else overwrites it (unless it was a deliberate push, handled above).
            if is_replaying && !is_deliberate && last_active_device.as_ref() == Some(&device_path) {
                if let Some((code, _)) = analog {
                    if let Some(&held) = injected_axes.get(&code) {
                        if value != held {
                            if inject(&mut write_devices, &mut ignore_injected, &device_path, EV_ABS, code, held) {
                                axis_overrides += 1;
                            }
                        }
                    }
                }
            }

            if state == State::Recording && !is_record_btn && !is_replay_btn {
                if recorded_events.len() >= MAX_EVENTS {
                    state = State::Idle;
                    info!("Macro size limit reached. Stopped recording.");
                    let elapsed = record_start_time.elapsed();
                    let tail = analog_recorder.finish(elapsed, |c| range_for(&axis_ranges, &last_active_device, c));
                    recorded_events.extend(tail);
                    save_macro(&active_macro_path, &recorded_events);
                } else if let Some((code, range)) = analog {
                    if let Some(ev) = analog_recorder.record(record_start_time.elapsed(), code, value, range) {
                        recorded_events.push(ev);
                    }
                } else {
                    recorded_events.push(MacroEvent::from_evdev(record_start_time.elapsed(), event));
                }
            }
        }

        if let Some(held) = record_held_since {
            if !record_triggered_this_press && held.elapsed() >= Duration::from_millis(config.hold_duration_ms) {
                if replay_held_since.is_none() {
                    record_triggered_this_press = true;
                    if state == State::Idle {
                        state = State::Recording;
                        recorded_events.clear();
                        record_start_time = Instant::now();
                        if is_replaying {
                            pending_recenter = true;
                        }
                        is_replaying = false;
                        recorded_events.extend(analog_recorder.start(&axis_state));
                        info!("Started recording to {:?} (hold {}ms)", active_macro_path, config.hold_duration_ms);
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
            }
        }

        if let Some(held) = replay_held_since {
            if !replay_triggered_this_press && held.elapsed() >= Duration::from_millis(config.hold_duration_ms) {
                if record_held_since.is_none() {
                    replay_triggered_this_press = true;
                    if state == State::Idle {
                        if recorded_events.is_empty() {
                            error!("Playback failed: no recorded inputs for {:?}", active_macro_path);
                        } else {
                            is_replaying = true;
                            replay_index = 0;
                            current_iteration = 0;
                            replay_start_time = Instant::now();
                            axis_overrides = 0;
                            info!("Started playback from {:?} (hold {}ms)", active_macro_path, config.hold_duration_ms);
                        }
                    }
                }
            }
        }

        if is_replaying && replay_index < recorded_events.len() {
            let next_event = &recorded_events[replay_index];
            if Instant::now() >= replay_start_time + Duration::from_millis(next_event.delay_ms) {
                if let Some(device_path) = last_active_device.clone() {
                    let (t, c, v) = (next_event.event_type, next_event.code, next_event.value);
                    if inject(&mut write_devices, &mut ignore_injected, &device_path, t, c, v)
                        && t == EV_ABS
                        && is_analog_axis(c)
                    {
                        injected_axes.insert(c, v);
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

        // Never leave a stick or trigger held after playback ends or is cancelled.
        if pending_recenter && !is_replaying {
            pending_recenter = false;
            if axis_overrides > 0 {
                info!("Held analog axes were overwritten by another writer {} time(s) and re-asserted.", axis_overrides);
                axis_overrides = 0;
            }
            if let Some(device_path) = last_active_device.clone() {
                for (code, last) in injected_axes.drain() {
                    let rest = range_for(&axis_ranges, &last_active_device, code).rest();
                    if last != rest {
                        inject(&mut write_devices, &mut ignore_injected, &device_path, EV_ABS, code, rest);
                    }
                }
            }
            injected_axes.clear();
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
        let mut r = AnalogRecorder::new();
        assert!(r.start(&HashMap::new()).is_empty());
        let t = Duration::from_millis(10);
        // drift (5%) and a light touch (15%): nothing
        assert!(r.record(t, ABS_X, 1600, STICK).is_none());
        assert!(r.record(t, ABS_X, 4900, STICK).is_none());
        // 20%: level 1 (25%)
        assert_eq!(r.record(t, ABS_X, 6550, STICK).unwrap().value, 8191);
        // hovering around the 37.5% boundary: stays on level 1
        for v in [12000, 12700, 11800, 13500] {
            assert!(r.record(t, ABS_X, v, STICK).is_none(), "{}", v);
        }
        // clearly at 50%: level 2
        assert_eq!(r.record(t, ABS_X, 16400, STICK).unwrap().value, 16383);
        // full: level 4, exact max
        assert_eq!(r.record(t, ABS_X, 32700, STICK).unwrap().value, 32767);
        // released to near centre: exact rest
        let ev = r.record(t, ABS_X, -1000, STICK).unwrap();
        assert_eq!((ev.event_type, ev.code, ev.value), (EV_ABS, ABS_X, 0));
        // other direction
        assert_eq!(r.record(t, ABS_X, -32768, STICK).unwrap().value, -32768);
    }

    #[test]
    fn full_sweep_is_compact() {
        let mut r = AnalogRecorder::new();
        r.start(&HashMap::new());
        let mut n = 0;
        for v in (-32768..=32767).step_by(16) {
            if r.record(Duration::ZERO, ABS_RX, v, STICK).is_some() {
                n += 1;
            }
        }
        assert!(n == 9, "sweep produced {} events", n);
    }

    #[test]
    fn start_captures_held_axes_and_finish_recenters() {
        let mut r = AnalogRecorder::new();
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
}
