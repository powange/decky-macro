# Decky Macro

A Decky Loader plugin for the Steam Deck that allows you to easily record and replay precise gamepad input macros. 

Built with a blazing-fast, low-level Rust daemon that interfaces directly with the Linux `evdev` subsystem, ensuring 1:1 input reproduction without lag or dropped inputs.

## Installing this fork (analog support)

This fork adds analog stick and trigger recording. On the Steam Deck, open Decky settings > Developer > *Install plugin from URL* and paste:

```
https://github.com/powange/decky-macro/releases/latest/download/decky-macro.zip
```

The same URL always points to the latest release, so re-installing from it updates the plugin. Releases are built by GitHub Actions (`.github/workflows/release.yml`) on every push to `main` whose `package.json` version has no release yet: bump the version to publish a new one.

## Features

- **Live Macro Recording:** Instantly record gamepad sequences directly during gameplay.
- **Perfect Playback:** Replays macros at the exact speed and duration they were recorded.
- **Native Decky UI:** Configure your trigger buttons, slots, and playback loops natively from the SteamOS Quick Access Menu.
- **Configurable Trigger Hold:** Choose how long the Record/Replay buttons must be held before they activate — anywhere from **0 to 2 seconds** in 100 ms steps (default **0.5s**). Set it to **0** to trigger instantly on press.
- **Echo Cancellation:** Deep `evdev` injection ensures macros seamlessly integrate with Gamescope without confusing Steam Input.
- **Manual Slot System:** Create, name, and manage multiple macro slots. Your active slot seamlessly switches in the background.
- **Auto-Cancellation:** Immediately abort a running macro loop by pressing any physical D-Pad or Face button (if enabled in the slot config).
- **Background Daemon:** Operates independently of the Quick Access Menu, meaning you can record and replay with zero performance impact.

## Limitations & Best Practices

- **Analog Inputs:** Sticks (`ABS_X`/`ABS_Y`, `ABS_RX`/`ABS_RY`) and triggers (`ABS_Z`/`ABS_RZ`, or `ABS_HAT2X`/`ABS_HAT2Y` on the native Deck device) are recorded from gamepads only (motion sensors and touchscreen are ignored). They are quantized to **4 steps per direction** (25/50/75/100%), with hysteresis so a stick held on a boundary doesn't flap; only step changes are stored. Every macro ends with the axes back at rest. During playback, pushing a stick or trigger past 50% cancels the macro (when cancel-on-input is enabled). Buttons are stored as a press and a release, so hold durations are replayed exactly.
- **Per-slot analog options:** *Record analog sticks* (on by default) and *Sticks: 4 directions only*, which keeps only the dominant axis of each stick (up/down/left/right) while recording.
- **Trigger modes:** each trigger (record, replay, picker) fires on a *long press* or on *repeated presses* (2-10 presses in a row). Each trigger has its own hold time (long press) or max time between presses (repeated presses); only the setting of the chosen mode is shown.
- **Macro picker:** an optional third trigger shows the list of recorded macros in the middle of the screen (an X11 window tagged `GAMESCOPE_EXTERNAL_OVERLAY`, drawn with cairo by `overlay/picker.py`, same technique as powange/decky-timer). While it is open the daemon grabs the gamepad so the game doesn't see the navigation: D-pad or left stick to choose, A to play (with that slot's loops and cancel settings), B or the picker button to close. The selected slot is restored after playback. The overlay window only exists while the menu is shown: gamescope composites a single external overlay at a time, so a permanent window would hide other plugins' overlays (e.g. Decky Timer).
- **Slots at a glance:** the slot list shows whether each slot is empty and how long its macro lasts; *Preview macro* lists its events (time, input, action). Each slot also has a playback speed (0.5x to 2x).
- **Player takes over:** during playback, pushing a stick past 25% stops the macro from re-asserting that axis; past 50% (or a button) cancels when *Cancel on input* is on.
- **Diagnostic** view (version, triggers, last daemon log lines) and **French / English** interface (*Language* in Settings, *Auto* follows Steam).
- **Hot reload:** changing the active slot or the settings no longer restarts the daemon: the plugin writes `active_slot.json` / `config.json` and sends SIGHUP; the daemon applies them once idle (never mid-recording or mid-playback). SIGTERM stops it cleanly: held inputs are released and a recording in progress is saved.
- **Remembers *Enable Decky Macro*:** the daemon starts again after a reboot if it was enabled.
- **System keys ignored:** volume, mute, power and brightness keys never cancel a playback and are never recorded.
- **Clean stop:** when a playback finishes or is cancelled, every button, stick and trigger still held by the macro is released. The daemon logs playback stats (iterations, injected events, re-asserts, cleanup time) and warns if its main loop stalls.
- **Back buttons as triggers:** Steam's virtual gamepad has no L4/L5/R4/R5, and hid-steam disables its own evdev device while Steam runs, so the daemon reads the back buttons from the Deck controller's raw HID reports (`/dev/hidraw*`, VID 28DE / PID 1205, report type 9) alongside Steam, read-only. They can be chosen or detected as record/replay triggers and cancel playback, but are not recorded. *Detect record/replay button* in Settings learns any other key too (touches, mouse buttons, A and B are ignored).
- **Hardware Triggers:** Make sure your physical buttons (like the back paddles `L4`/`R4`) are mapped to the configured trigger buttons (e.g., `L3`/`R3`) via Steam Input.

## Macro Data Format & Limits

- **Maximum Size:** Each macro can record a maximum of **20,000 events**. Once this limit is hit, recording stops automatically and saves.
- **Format:** Macros are saved in `~/homebrew/settings/decky-macro/macro_<slot>.json`. They are simple JSON arrays containing the delay (in milliseconds since the last event), and the raw `evdev` event type, code, and value.
  ```json
  [
    { "delay_ms": 150, "event_type": 1, "code": 304, "value": 1 },
    { "delay_ms": 80, "event_type": 1, "code": 304, "value": 0 }
  ]
  ```

## Installation / Building

Because this plugin contains a compiled Rust backend, you must build it using Node.js and Cargo. 

1. Clone this repository into your development environment.
2. Ensure you have Node.js, `pnpm`, and the Rust toolchain installed.
3. Install dependencies:
   ```bash
   pnpm install
   ```
4. Build and assemble a deployable plugin folder:
   ```bash
   pnpm run build             # frontend bundle -> dist/
   pnpm run build:backend     # Rust daemon -> bin/   (or build:backend:static)
   pnpm run package           # -> target/decky-macro/
   ```
5. Move the resulting `target/decky-macro` folder to your Steam Deck's `~/homebrew/plugins/` directory and restart your Deck.

### How the backend is built

`pnpm run build` only builds the **frontend** (`rollup -c`). The daemon is built by `pnpm run build:backend` locally, or by the **store CI** through `backend/Dockerfile` (image `ghcr.io/steamdeckhomebrew/holo-toolchain-rust`), which mounts `backend/` → `/backend` and the output → `/backend/out`, then runs `entrypoint.sh` → `build.sh`. That container is **SteamOS-based**, so the store ships a binary linked against the Deck's glibc.

Building locally on a newer-libc host (WSL/Ubuntu) and running on the Deck can fail with `GLIBC_x.xx not found`. Use the static musl build instead — it runs anywhere:

```bash
pnpm run build:backend         # native (host libc)
pnpm run build:backend:static  # static musl
```

`build:backend:static` installs the musl Rust std if needed, and links via Rust's bundled `rust-lld` when no C compiler is present. You only need it for local testing — the store CI builds a SteamOS-glibc binary.

*(Note: If building for official release, remember to use the official `decky-cli` docker builder).*

## Usage

1. Open the Quick Access Menu (QAM) on your Steam Deck and navigate to the Decky Macro plugin.
2. With the daemon **stopped**, click **Settings** to choose your **Record Trigger** and **Replay Trigger** buttons (defaults to L3 / R3) and the **Trigger hold time**. The Settings button is disabled while the daemon is running.
3. Turn on the **Enable Decky Macro** toggle to start the daemon — the saved trigger settings are applied on start.
4. In a game, hold your **Record** button for the configured hold time to start capturing inputs. Hold it again to stop.
5. Hold the **Replay** button for the configured hold time to play back the exact sequence.

### Trigger Settings
The **Record Trigger**, **Replay Trigger** and **Trigger hold time** are configured from the **Settings** dialog (the button above *Active Macro Slot*). Because the daemon reads these values at startup, the **Settings button is disabled while the daemon is running** — stop the daemon, change the settings, then start it again.

**Trigger hold time** controls how long the Record/Replay buttons must be held before they activate:

- **Range:** `0` to `2` seconds, adjustable in `0.1s` (100 ms) steps.
- **Default:** `0.5` seconds.
- **`0` seconds:** the trigger fires **immediately** on press (no hold required).

The values are saved to the global config (`config.json` → `trigger_record_code`, `trigger_replay_code`, `hold_duration_ms`).

### Notifications
In the **Notifications** section of the plugin:

- **Show info toasts** — when enabled (the default), status toasts are shown for *Recording Started/Saved*, *Playback Started/Finished* and *Playback Cancelled*.
- **Error toasts are always shown**, regardless of this setting, for example:
  - *No inputs recorded — nothing saved*: a recording finished without capturing any button presses. The previous macro (if any) is kept.
  - *No macro recorded for this slot*: replay was triggered but the active slot has no recorded inputs.

### Loop Mode
In the slot settings (click "Edit Selected Slot"), you can configure how many times the macro should run:
- `1-50`: Run exactly N times.
- `0`: Infinite Loop (Will run until you manually interrupt it by pressing another button).
