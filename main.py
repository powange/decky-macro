import sys
import os
import subprocess
import json
import threading
import asyncio
import traceback
import pwd
import time
import signal
import select
import socket
from collections import deque
import decky_plugin

# Readable names for the macro preview (Steam virtual gamepad codes).
KEY_NAMES = {
    304: "A", 305: "B", 307: "X", 308: "Y", 310: "L1", 311: "R1", 312: "L2", 313: "R2",
    314: "View", 315: "Menu", 316: "Steam", 317: "L3", 318: "R3",
    544: "D-pad up", 545: "D-pad down", 546: "D-pad left", 547: "D-pad right",
}
AXIS_NAMES = {0: "Left stick X", 1: "Left stick Y", 2: "L2", 3: "Right stick X", 4: "Right stick Y",
              5: "R2", 16: "D-pad X", 17: "D-pad Y", 20: "R2", 21: "L2"}


def _read_macro(path):
    """Events of a macro file as (delay_ms, type, code, value) tuples."""
    try:
        with open(path, "r") as f:
            raw = json.load(f)
    except Exception:
        return []
    events = []
    for item in raw if isinstance(raw, list) else []:
        parts = str(item).split(":")
        if len(parts) == 4:
            try:
                events.append(tuple(int(x) for x in parts))
            except ValueError:
                pass
    return events

# ---- Decky Overlay ----------------------------------------------------------
# gamescope shows a single external overlay window, so the macro picker is drawn
# by the Decky Overlay plugin (powange/decky-overlay), shared with Decky Timer
# and others. Protocol: JSON lines on a Unix socket; our blocks disappear when
# the connection closes.
OVERLAY_SOCKET = "/tmp/decky-overlay/overlay.sock"
OVERLAY_PLUGIN_NAME = "Decky Overlay"
OVERLAY_RETRY = 2.0          # seconds between connection attempts
MENU_TEXT = {
    "en": ("Choose a macro", "D-pad / stick: choose · A: play · B: close"),
    "fr": ("Choisir une macro", "Croix / stick : choisir · A : lancer · B : fermer"),
}


class OverlayClient:
    """Blocking client for Decky Overlay, used from the daemon log thread."""

    def __init__(self, on_error=None):
        self.sock = None
        self.lock = threading.Lock()
        self.last_attempt = 0.0
        self.error = ""
        self.features = []
        self.on_error = on_error

    @property
    def connected(self):
        return self.sock is not None

    def _connect(self, force=False):
        """Connects if needed (lock held). Returns True when connected."""
        if self.sock is not None:
            return True
        if not force and time.time() - self.last_attempt < OVERLAY_RETRY:
            return False
        self.last_attempt = time.time()
        if not os.path.exists(OVERLAY_SOCKET):
            return False
        sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            sock.settimeout(2)
            sock.connect(OVERLAY_SOCKET)
            sock.settimeout(None)
            self.sock = sock
            self._write({"op": "hello", "client": "decky-macro", "name": "Decky Macro", "protocol": 1})
        except OSError as e:
            self.sock = None
            sock.close()
            self.error = f"Decky Overlay: {e}"
            return False
        self.error = ""
        threading.Thread(target=self._read, args=(sock,), daemon=True).start()
        return True

    def _write(self, msg):
        # Never block the log thread for long on a stuck server.
        _, writable, _ = select.select([], [self.sock], [], 0.5)
        if not writable:
            raise OSError("Decky Overlay not responding")
        self.sock.sendall((json.dumps(msg) + "\n").encode())

    def _read(self, sock):
        try:
            for line in sock.makefile("rb"):
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue
                if msg.get("op") == "welcome":
                    self.features = list(msg.get("features") or [])
                elif msg.get("op") == "error":
                    self.error = f"Decky Overlay: {msg.get('error')}"
                    decky_plugin.logger.warning(self.error)
                    if self.on_error:
                        self.on_error(str(msg.get("error") or ""))
        except (OSError, ValueError):
            pass
        with self.lock:
            if self.sock is sock:
                self._drop()

    def _drop(self):
        if self.sock is not None:
            try:
                self.sock.close()
            except OSError:
                pass
        self.sock = None

    def ensure(self):
        with self.lock:
            return self._connect()

    def send(self, msg, force=False):
        """Sends one message, connecting first if needed. False if not sent."""
        with self.lock:
            if not self._connect(force):
                return False
            try:
                self._write(msg)
                return True
            except OSError as e:
                self.error = f"Decky Overlay: {e}"
                self._drop()
                return False

    def close(self):
        with self.lock:
            self._drop()


def _overlay_installed():
    """True if a plugin named "Decky Overlay" is in Decky's plugins folder."""
    home = getattr(decky_plugin, "DECKY_HOME", "") or "/home/deck/homebrew"
    plugins = os.path.join(home, "plugins")
    try:
        names = os.listdir(plugins)
    except OSError:
        return False
    for name in names:
        try:
            with open(os.path.join(plugins, name, "plugin.json")) as f:
                if json.load(f).get("name") == OVERLAY_PLUGIN_NAME:
                    return True
        except (OSError, ValueError, AttributeError):
            continue
    return False


# Default global config. `hold_duration_ms` is how long the record/replay
# trigger buttons must be held before activating (0 = trigger on press).
# `show_info_toasts` controls whether informational status toasts are shown;
# error toasts are always shown regardless of this setting.
DEFAULT_CONFIG = {
    "trigger_record_code": 317,
    "trigger_replay_code": 318,
    "hold_duration_ms": 500,
    "record_trigger_mode": "hold",
    "replay_trigger_mode": "hold",
    "record_press_count": 3,
    "replay_press_count": 3,
    "press_window_ms": 400,
    "trigger_picker_code": 0,
    "picker_trigger_mode": "hold",
    "picker_press_count": 3,
    "daemon_enabled": False,
    "language": "auto",
    "show_info_toasts": True,
}

class Plugin:
    # Last daemon log lines, for the Diagnostic view.
    log_lines = deque(maxlen=200)
    process = None
    # Shared between the daemon log thread and the frontend calls.
    lock = threading.Lock()
    daemon_state = {}          # last STATE line of the daemon
    stats_version = 0          # bumped when a macro file changes
    detect_event = threading.Event()
    detect_result = None
    ui_lang = "en"             # menu language, sent by the frontend

    def __init__(self):
        self.config_dir = decky_plugin.DECKY_PLUGIN_SETTINGS_DIR
        self.config_path = os.path.join(self.config_dir, "config.json")
        self.data_path = os.path.join(self.config_dir, "data.json")
        self.bin_path = os.path.join(decky_plugin.DECKY_PLUGIN_DIR, "bin", "decky-macro-daemon")
        self.notifications = []
        self.overlay = OverlayClient(on_error=self._overlay_error)
        self.menu_open = False
        self.menu_warned = False

        if not os.path.exists(self.config_dir):
            os.makedirs(self.config_dir)

        if os.path.exists(self.bin_path):
            os.chmod(self.bin_path, 0o755)

        # Ensure default config
        if not os.path.exists(self.config_path):
            with open(self.config_path, "w") as f:
                json.dump(dict(DEFAULT_CONFIG), f, indent=4)

        # Ensure default data.json
        if not os.path.exists(self.data_path):
            with open(self.data_path, "w") as f:
                json.dump({
                    "active_slot": "0",
                    "slots": {
                        "0": { "name": "Default", "loops": 0, "cancel_on_input": True, "record_analog": True, "cardinal_only": False }
                    }
                }, f)

    def _get_data(self):
        if not os.path.exists(self.data_path): return {}
        with open(self.data_path, "r") as f:
            return json.load(f)

    def _write_slot_file(self, data) -> str:
        """Settings of the active slot, read by the daemon at start and on SIGHUP."""
        active_slot = str(data.get("active_slot", "0"))
        sc = data.get("slots", {}).get(active_slot, {})
        path = os.path.join(self.config_dir, "active_slot.json")
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump({
                "macro_file": os.path.join(self.config_dir, f"macro_{active_slot}.json"),
                "loops": int(sc.get("loops", 0)),
                "cancel_on_input": bool(sc.get("cancel_on_input", True)),
                "record_analog": bool(sc.get("record_analog", True)),
                "cardinal_only": bool(sc.get("cardinal_only", False)),
                "speed": float(sc.get("speed", 1.0)),
                "record_append": bool(sc.get("record_append", False)),
                "loop_pause_ms": int(sc.get("loop_pause_ms", 0)),
                "max_minutes": int(sc.get("max_minutes", 0)),
                "jitter_ms": int(sc.get("jitter_ms", 0)),
            }, f)
        os.replace(tmp, path)
        return path

    def _reload_daemon(self):
        """Makes a running daemon re-read its config and active slot (SIGHUP).
        It applies the change once idle, so a recording or a playback in progress
        is never lost (instead of the old restart)."""
        data = self._get_data()
        self._write_slot_file(data)
        self._write_picker_file(data)
        if Plugin.process is not None and Plugin.process.poll() is None:
            Plugin.process.send_signal(signal.SIGHUP)
            decky_plugin.logger.info("Daemon asked to reload its settings")
            self._ensure_overlay()

    def start_daemon(self) -> bool:
        self.stop_daemon()
        if not os.path.exists(self.bin_path):
            decky_plugin.logger.error(f"Cannot start daemon: binary not found at {self.bin_path}")
            return False
            
        try:
            data = self._get_data()
            active_slot = data.get("active_slot", "0")
            slot_config = data.get("slots", {}).get(active_slot, {})
            
            loops = str(slot_config.get("loops", 0))
            cancel = "true" if slot_config.get("cancel_on_input", True) else "false"
            record_analog = "true" if slot_config.get("record_analog", True) else "false"
            cardinal_only = "true" if slot_config.get("cardinal_only", False) else "false"
            macro_file = os.path.join(self.config_dir, f"macro_{active_slot}.json")
            picker_file = self._write_picker_file(data)
            slot_file = self._write_slot_file(data)

            decky_plugin.logger.info(f"Starting decky-macro-daemon daemon for slot {active_slot}...")
            Plugin.process = subprocess.Popen(
                [
                    self.bin_path, 
                    "--config", self.config_path, 
                    "--macro-file", macro_file,
                    "--loops", loops,
                    "--cancel-on-input", cancel,
                    "--record-analog", record_analog,
                    "--cardinal-only", cardinal_only,
                    "--picker-file", picker_file,
                    "--slot-file", slot_file,
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT
            )
            
            def log_output(pipe):
                with pipe:
                    for line in iter(pipe.readline, b''):
                        # Never let one bad line kill this thread: if nobody reads
                        # the pipe, the daemon blocks on its next log write.
                        try:
                            handle_line(line)
                        except Exception as e:
                            try:
                                decky_plugin.logger.error(f"Daemon log handling failed: {e}")
                            except Exception:
                                pass

            def notify(message, error=False):
                self._notify(message, error)

            def handle_line(line):
                        line_str = line.decode('utf-8', errors='replace').strip()
                        if not line_str.startswith("PICKER_"):
                            Plugin.log_lines.append(line_str)

                        # Status for the panel (F10/F11), not logged.
                        if line_str.startswith("STATE "):
                            with Plugin.lock:
                                Plugin.daemon_state = json.loads(line_str[len("STATE "):])
                            return
                        # Trigger detection while the daemon runs (F12).
                        if line_str.startswith("DETECTED ") or line_str == "DETECT_TIMEOUT":
                            Plugin.detect_result = line_str
                            Plugin.detect_event.set()
                            return
                        # Picker menu messages go to the overlay, not the log.
                        if line_str.startswith("PICKER_SHOW "):
                            self._show_menu(json.loads(line_str[len("PICKER_SHOW "):]))
                            return
                        if line_str == "PICKER_HIDE":
                            self._hide_menu()
                            return
                        decky_plugin.logger.info(f"[Rust] {line_str}")

                        if "Started recording to" in line_str:
                            notify("Recording Started")
                        elif "Stopped recording. Saved" in line_str:
                            Plugin.stats_version += 1
                            notify("Recording Saved")
                        elif "Recording stopped with no inputs" in line_str:
                            notify("No inputs recorded - nothing saved", error=True)
                        elif "Started playback from" in line_str:
                            notify("Playback Started")
                        elif "Playback finished." in line_str:
                            notify("Playback Finished")
                        elif "Playback cancelled by physical input" in line_str:
                            notify("Playback Cancelled")
                        elif "Playback failed: no recorded inputs" in line_str:
                            notify("No macro recorded for this slot", error=True)
                        elif "Playback failed: no gamepad found" in line_str:
                            notify("No gamepad found to play the macro on", error=True)
                        elif "Trigger not available: record" in line_str:
                            notify("Record button is not seen by the daemon - pick another one", error=True)
                        elif "Trigger not available: replay" in line_str:
                            notify("Replay button is not seen by the daemon - pick another one", error=True)
                        elif "Trigger not available: picker" in line_str:
                            notify("Picker button is not seen by the daemon - pick another one", error=True)
                        elif "Trigger available again" in line_str:
                            notify("Trigger button detected again")
                        elif "Macro size limit reached" in line_str:
                            notify("Macro size limit reached - recording saved", error=True)
                        elif "Picker: no recorded macro" in line_str:
                            notify("No recorded macro to choose from", error=True)
                        elif "Picker: cannot open gamepad" in line_str:
                            notify("Macro picker: cannot read the gamepad", error=True)
                        elif "Playback stopped:" in line_str and "limit reached" in line_str:
                            notify("Playback stopped: time limit reached")
                        
            threading.Thread(target=log_output, args=(Plugin.process.stdout,), daemon=True).start()
            self._ensure_overlay()
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to start daemon: {e}\n{traceback.format_exc()}")
            return False

    # ------------------------------------------------------------------ #
    # Macro picker
    # ------------------------------------------------------------------ #

    def _write_picker_file(self, data) -> str:
        """Lists every slot for the daemon's macro picker."""
        path = os.path.join(self.config_dir, "picker.json")
        items = []
        slots = data.get("slots", {})
        for sid in sorted(slots, key=lambda k: int(k) if str(k).isdigit() else 0):
            sc = slots[sid]
            items.append({
                "name": sc.get("name") or f"Slot {sid}",
                "path": os.path.join(self.config_dir, f"macro_{sid}.json"),
                "loops": int(sc.get("loops", 0)),
                "cancel_on_input": bool(sc.get("cancel_on_input", True)),
                "speed": float(sc.get("speed", 1.0)),
                "loop_pause_ms": int(sc.get("loop_pause_ms", 0)),
                "max_minutes": int(sc.get("max_minutes", 0)),
                "jitter_ms": int(sc.get("jitter_ms", 0)),
            })
        with open(path, "w") as f:
            json.dump(items, f)
        return path

    def _ensure_overlay(self):
        """Connects to Decky Overlay ahead of time when a picker trigger is set,
        so the first menu shows at once."""
        try:
            with open(self.config_path, "r") as f:
                wanted = bool(json.load(f).get("trigger_picker_code"))
        except Exception:
            wanted = False
        if wanted and Plugin.process is not None and Plugin.process.poll() is None:
            self.overlay.ensure()

    def _show_menu(self, menu):
        """Draws the picker through Decky Overlay (input stays with the daemon)."""
        title, hint = MENU_TEXT.get(Plugin.ui_lang, MENU_TEXT["en"])
        items = [str(i) for i in menu.get("items", [])] or ["-"]
        sent = self.overlay.send({"op": "menu", "id": "picker", "title": title, "hint": hint,
                                  "items": items, "selected": int(menu.get("selected", 0))},
                                 force=not self.menu_open)
        self.menu_open = True
        if not sent and not self.menu_warned:
            self.menu_warned = True
            if _overlay_installed():
                self._notify("Macro picker: Decky Overlay is not running", error=True)
            else:
                self._notify("Macro picker needs the Decky Overlay plugin", error=True)

    def _hide_menu(self):
        self.menu_open = False
        self.menu_warned = False
        self.overlay.send({"op": "clear", "id": "picker"})

    def _overlay_error(self, error):
        if "unknown op" in error and not self.menu_warned:
            self.menu_warned = True
            self._notify("Macro picker: update Decky Overlay", error=True)

    def _overlay_status(self):
        if self.overlay.connected:
            if self.overlay.features and "menu" not in self.overlay.features:
                return "outdated"
            return "connected"
        if not hasattr(self, "_overlay_checked") or time.time() - self._overlay_checked > 10:
            self._overlay_is_installed = _overlay_installed()
            self._overlay_checked = time.time()
        return "not_running" if self._overlay_is_installed else "not_installed"

    def stop_daemon(self):
        self.menu_open = False
        self.overlay.close()
        if Plugin.process:
            decky_plugin.logger.info("Stopping decky-macro-daemon daemon...")
            # SIGTERM: the daemon releases held inputs and saves a recording
            # in progress, then exits; kill it if it takes too long.
            Plugin.process.terminate()
            try:
                Plugin.process.wait(timeout=3)
            except subprocess.TimeoutExpired:
                decky_plugin.logger.warning("Daemon did not stop in 3 s, killing it")
                Plugin.process.kill()
                Plugin.process.wait(timeout=2)
            Plugin.process = None

    async def detect_trigger(self, which: str) -> dict:
        """Waits for the next key press on any input device and saves it as the
        record or replay trigger. Lets back buttons work once Steam Input maps
        them to a keyboard key (the Deck does not expose them otherwise)."""
        if which not in ("record", "replay", "picker"):
            return {"error": "invalid trigger"}
        if self.get_daemon_state_sync():
            # The running daemon sees every device: ask it (SIGUSR1) and wait for
            # its DETECTED / DETECT_TIMEOUT line, read by the log thread.
            Plugin.detect_event.clear()
            Plugin.detect_result = None
            Plugin.process.send_signal(signal.SIGUSR1)
            loop = asyncio.get_running_loop()
            await loop.run_in_executor(None, Plugin.detect_event.wait, 12)
            line = [Plugin.detect_result or ""]
        else:
            try:
                proc = await asyncio.create_subprocess_exec(
                    self.bin_path, "--detect-button", "10000",
                    stdout=asyncio.subprocess.PIPE,
                    stderr=asyncio.subprocess.DEVNULL,
                )
                out, _ = await asyncio.wait_for(proc.communicate(), timeout=15)
            except Exception as e:
                decky_plugin.logger.error(f"Button detection failed: {e}\n{traceback.format_exc()}")
                return {"error": str(e)}
            line = out.decode("utf-8", errors="replace").strip().splitlines()[-1:] or [""]
        parts = line[0].split(" ", 3)
        if len(parts) < 3 or parts[0] != "DETECTED":
            self._notify("No button press detected", error=True)
            return {"error": "timeout"}

        code, label = int(parts[1]), parts[2]
        device = parts[3] if len(parts) > 3 else "?"
        config = await self.get_config()
        config[f"trigger_{which}_code"] = code
        config[f"trigger_{which}_label"] = label
        await self.save_config(config)
        decky_plugin.logger.info(f"Detected {which} trigger: {label} ({code}) from {device}")
        self._notify(f"{which.capitalize()} trigger set to {label}")
        return {"code": code, "label": label, "device": device}

    # ------------------------------------------------------------------ #
    # Slot info, macro preview, diagnostics
    # ------------------------------------------------------------------ #

    async def get_slot_stats(self) -> dict:
        """Number of events and duration of each slot's macro."""
        stats = {}
        for sid in self._get_data().get("slots", {}):
            events = _read_macro(os.path.join(self.config_dir, f"macro_{sid}.json"))
            stats[sid] = {"events": len(events), "duration_ms": events[-1][0] if events else 0}
        return stats

    async def get_macro_preview(self, slot_id: str) -> dict:
        """A readable timeline of a slot's macro (first 300 events)."""
        events = _read_macro(os.path.join(self.config_dir, f"macro_{slot_id}.json"))
        rows = []
        for idx, (delay, etype, code, value) in enumerate(events[:300]):
            if etype == 1:
                name = KEY_NAMES.get(code, f"key {code}")
                what = "press" if value == 1 else "release" if value == 0 else f"repeat"
            elif etype == 3:
                name = AXIS_NAMES.get(code, f"axis {code}")
                if code == 16:
                    what = {-1: "left", 0: "center", 1: "right"}.get(value, str(value))
                elif code == 17:
                    what = {-1: "up", 0: "center", 1: "down"}.get(value, str(value))
                elif code in (2, 5, 20, 21):
                    what = f"{round(value * 100 / 255)}%"
                else:
                    what = f"{round(value * 100 / 32767)}%" if value else "center"
            else:
                continue
            rows.append({"i": idx, "t": delay, "input": name, "action": what})
        return {"count": len(events), "duration_ms": events[-1][0] if events else 0, "rows": rows}

    def get_daemon_state_sync(self) -> bool:
        return Plugin.process is not None and Plugin.process.poll() is None

    async def get_status(self) -> dict:
        """Daemon state (F10/F11) and a counter that changes when a macro file
        changes, so the panel only re-reads the slot stats when needed (P8)."""
        running = self.get_daemon_state_sync()
        with Plugin.lock:
            state = dict(Plugin.daemon_state) if running else {}
        return {"running": running, "state": state, "stats_version": Plugin.stats_version,
                "overlay": self._overlay_status()}

    async def set_ui_language(self, lang: str) -> bool:
        """Language of the in-game picker (the frontend resolves "auto")."""
        Plugin.ui_lang = "fr" if lang == "fr" else "en"
        return True

    async def recheck_overlay(self) -> str:
        """After installing Decky Overlay: connect now instead of waiting."""
        self._overlay_checked = 0.0
        if Plugin.process is not None and Plugin.process.poll() is None:
            with self.overlay.lock:
                self.overlay._connect(force=True)
        return self._overlay_status()

    # --- A6: a slot per game -------------------------------------------------

    async def set_running_game(self, app_id: str, name: str = "") -> str:
        """Called by the panel when the running game changes: selects the slot
        associated with that game, if any. Returns the selected slot id or ""."""
        if not app_id:
            return ""
        data = self._get_data()
        for sid, sc in data.get("slots", {}).items():
            if str(sc.get("game_id", "")) == str(app_id):
                if str(data.get("active_slot")) != str(sid):
                    data["active_slot"] = sid
                    await self.save_slots_data(data)
                    self._notify(f"Slot selected for {name or app_id}: {sc.get('name', sid)}")
                return str(sid)
        return ""

    # --- A7: export / import ---------------------------------------------------

    def _exports_dir(self) -> str:
        home = getattr(decky_plugin, "DECKY_USER_HOME", None)
        if not home:
            try:
                home = pwd.getpwnam(getattr(decky_plugin, "DECKY_USER", "deck")).pw_dir
            except Exception:
                home = os.path.expanduser("~")
        path = os.path.join(home, "decky-macro-exports")
        os.makedirs(path, exist_ok=True)
        self._give_to_user(path)
        return path

    def _give_to_user(self, path):
        """Files written by the root backend in the user's home belong to the user."""
        try:
            pw = pwd.getpwnam(getattr(decky_plugin, "DECKY_USER", "deck"))
            if os.geteuid() == 0:
                os.chown(path, pw.pw_uid, pw.pw_gid)
        except Exception:
            pass

    async def export_macro(self, slot_id: str) -> dict:
        data = self._get_data()
        sc = data.get("slots", {}).get(str(slot_id))
        if sc is None:
            return {"error": "unknown slot"}
        macro = os.path.join(self.config_dir, f"macro_{slot_id}.json")
        try:
            with open(macro, "r") as f:
                events = json.load(f)
        except Exception:
            return {"error": "empty"}
        safe = "".join(c if c.isalnum() or c in "-_ " else "_" for c in sc.get("name", f"slot {slot_id}")).strip() or f"slot_{slot_id}"
        path = os.path.join(self._exports_dir(), f"{safe}.json")
        settings = {k: v for k, v in sc.items() if k not in ("game_id", "game_name")}
        with open(path, "w") as f:
            json.dump({"format": "decky-macro/1", "name": sc.get("name"), "settings": settings, "events": events}, f, indent=1)
        self._give_to_user(path)
        return {"path": path}

    async def list_exports(self) -> list:
        folder = self._exports_dir()
        return sorted(f for f in os.listdir(folder) if f.endswith(".json"))

    async def import_macro(self, filename: str, slot_id: str) -> dict:
        folder = self._exports_dir()
        path = os.path.join(folder, os.path.basename(filename))
        try:
            with open(path, "r") as f:
                content = json.load(f)
            events = content["events"] if isinstance(content, dict) else content
            if not isinstance(events, list) or not all(isinstance(e, str) and e.count(":") == 3 for e in events):
                return {"error": "not a Decky Macro file"}
        except Exception as e:
            return {"error": str(e)}
        self._write_macro(slot_id, events)
        data = self._get_data()
        if isinstance(content, dict) and str(slot_id) in data.get("slots", {}):
            for k, v in (content.get("settings") or {}).items():
                if k != "name":
                    data["slots"][str(slot_id)][k] = v
            await self.save_slots_data(data)
        return {"events": len(events)}

    # --- A9: editing ---------------------------------------------------------------

    def _write_macro(self, slot_id, events):
        path = os.path.join(self.config_dir, f"macro_{slot_id}.json")
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(events, f)
        os.replace(tmp, path)
        Plugin.stats_version += 1
        self._reload_daemon()

    async def delete_macro_event(self, slot_id: str, index: int) -> dict:
        """Removes one event. Deleting a press also removes its matching
        release (and the other way round), so nothing stays held."""
        events = _read_macro(os.path.join(self.config_dir, f"macro_{slot_id}.json"))
        if not 0 <= index < len(events):
            return {"error": "index"}
        _, etype, code, value = events[index]
        remove = {index}
        if etype == 1 and value in (0, 1):
            step = 1 if value == 1 else -1
            i = index + step
            while 0 <= i < len(events):
                if events[i][1] == 1 and events[i][2] == code and events[i][3] == 1 - value:
                    remove.add(i)
                    break
                i += step
        kept = [e for i, e in enumerate(events) if i not in remove]
        self._write_macro(slot_id, [f"{d}:{t}:{c}:{v}" for d, t, c, v in kept])
        return {"removed": len(remove), "events": len(kept)}

    async def trim_macro_start(self, slot_id: str) -> dict:
        """Removes the wait before the first event."""
        events = _read_macro(os.path.join(self.config_dir, f"macro_{slot_id}.json"))
        if not events:
            return {"error": "empty"}
        first = events[0][0]
        self._write_macro(slot_id, [f"{d - first}:{t}:{c}:{v}" for d, t, c, v in events])
        return {"trimmed_ms": first}

    async def get_diagnostics(self) -> dict:
        running = await self.get_daemon_state()
        config = await self.get_config()
        return {
            "running": running,
            "version": await self.get_version(),
            "triggers": {k: config.get(f"trigger_{k}_label") or config.get(f"trigger_{k}_code")
                         for k in ("record", "replay", "picker")},
            "lines": list(Plugin.log_lines)[-40:],
        }

    async def get_version(self) -> str:
        version = os.environ.get("DECKY_PLUGIN_VERSION")
        if version:
            return version
        try:
            with open(os.path.join(decky_plugin.DECKY_PLUGIN_DIR, "package.json"), "r") as f:
                return json.load(f).get("version", "?")
        except Exception:
            return "?"

    async def get_notifications(self) -> list:
        with Plugin.lock:
            res, self.notifications = self.notifications, []
        return res

    def _notify(self, message, error=False):
        with Plugin.lock:
            self.notifications.append({"message": message, "error": error})

    async def get_config(self) -> dict:
        config = dict(DEFAULT_CONFIG)
        if os.path.exists(self.config_path):
            try:
                with open(self.config_path, "r") as f:
                    config.update(json.load(f))
            except Exception as e:
                decky_plugin.logger.error(
                    f"Failed to read config from {self.config_path}: {e}\n{traceback.format_exc()}"
                )
        return config

    async def save_config(self, config: dict) -> bool:
        # `daemon_enabled` is owned by set_daemon_state: keep the stored value so
        # a stale copy from the UI can't overwrite it. A running daemon reloads
        # the new settings (SIGHUP) once it is idle.
        config = dict(config)
        config["daemon_enabled"] = (await self.get_config()).get("daemon_enabled", False)
        ok = self._write_config(config)
        if ok:
            self._reload_daemon()
        return ok

    def _write_config(self, config: dict) -> bool:
        try:
            tmp = self.config_path + ".tmp"
            with open(tmp, "w") as f:
                json.dump(config, f, indent=4)
            os.replace(tmp, self.config_path)
            decky_plugin.logger.info("Config saved.")
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to save config to {self.config_path}: {e}\n{traceback.format_exc()}")
            return False

    async def get_slots_data(self) -> dict:
        return self._get_data()

    async def save_slots_data(self, data: dict) -> bool:
        try:
            # Delete the macro of removed slots, so a new slot reusing the id
            # doesn't inherit it.
            old_data = self._get_data()
            old_ids = set(old_data.get("slots", {}).keys())
            new_ids = set(data.get("slots", {}).keys())
            recording = Plugin.daemon_state.get("mode") == "recording" and self.get_daemon_state_sync()
            if recording and str(old_data.get("active_slot")) in (old_ids - new_ids):
                self._notify("Cannot delete the slot being recorded", error=True)
                return False
            for sid in old_ids - new_ids:
                macro = os.path.join(self.config_dir, f"macro_{sid}.json")
                if os.path.exists(macro):
                    os.remove(macro)
                    Plugin.stats_version += 1
                    decky_plugin.logger.info(f"Deleted macro of removed slot {sid}")
            tmp = self.data_path + ".tmp"
            with open(tmp, "w") as f:
                json.dump(data, f, indent=4)
            os.replace(tmp, self.data_path)
            # The daemon picks the change up without a restart (no lost recording).
            self._reload_daemon()
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to save slots data to {self.data_path}: {e}\n{traceback.format_exc()}")
            return False

    async def get_daemon_state(self) -> bool:
        if Plugin.process is None: return False
        return Plugin.process.poll() is None

    async def set_daemon_state(self, is_running: bool) -> bool:
        # Run in a worker thread: stopping waits for the daemon to clean up,
        # which must not block Decky's event loop.
        loop = asyncio.get_running_loop()
        if is_running:
            if not await loop.run_in_executor(None, self.start_daemon):
                raise Exception("Failed to start Decky Macro daemon. Check logs for details.")
        else:
            await loop.run_in_executor(None, self.stop_daemon)
        # Remember the choice so the daemon comes back after a reboot.
        config = await self.get_config()
        if config.get("daemon_enabled") != is_running:
            config["daemon_enabled"] = is_running
            self._write_config(config)
        return True

    async def _main(self):
        # Restart the daemon if it was enabled the last time.
        config = await self.get_config()
        if config.get("daemon_enabled"):
            decky_plugin.logger.info("Daemon was enabled last time, starting it")
            self.start_daemon()

    async def _unload(self):
        self.stop_daemon()

