import sys
import os
import subprocess
import json
import threading
import asyncio
import traceback
import decky_plugin

# Default global config. `hold_duration_ms` is how long the record/replay
# trigger buttons must be held before activating (0 = trigger on press).
# `show_info_toasts` controls whether informational status toasts are shown;
# error toasts are always shown regardless of this setting.
DEFAULT_CONFIG = {
    "trigger_record_code": 317,
    "trigger_replay_code": 318,
    "hold_duration_ms": 500,
    "show_info_toasts": True,
}

class Plugin:
    process = None

    def __init__(self):
        self.config_dir = decky_plugin.DECKY_PLUGIN_SETTINGS_DIR
        self.config_path = os.path.join(self.config_dir, "config.json")
        self.data_path = os.path.join(self.config_dir, "data.json")
        self.bin_path = os.path.join(decky_plugin.DECKY_PLUGIN_DIR, "bin", "decky-macro-daemon")
        self.notifications = []

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
                ],
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT
            )
            
            def log_output(pipe):
                def notify(message, error=False):
                    self.notifications.append({"message": message, "error": error})

                with pipe:
                    for line in iter(pipe.readline, b''):
                        line_str = line.decode('utf-8', errors='replace').strip()
                        decky_plugin.logger.info(f"[Rust] {line_str}")

                        if "Started recording to" in line_str:
                            notify("Recording Started")
                        elif "Stopped recording. Saved" in line_str:
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
                        
            threading.Thread(target=log_output, args=(Plugin.process.stdout,), daemon=True).start()
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to start daemon: {e}\n{traceback.format_exc()}")
            return False

    def stop_daemon(self):
        if Plugin.process:
            decky_plugin.logger.info("Stopping decky-macro-daemon daemon...")
            Plugin.process.terminate()
            Plugin.process.wait()
            Plugin.process = None

    async def detect_trigger(self, which: str) -> dict:
        """Waits for the next key press on any input device and saves it as the
        record or replay trigger. Lets back buttons work once Steam Input maps
        them to a keyboard key (the Deck does not expose them otherwise)."""
        if which not in ("record", "replay"):
            return {"error": "invalid trigger"}
        if await self.get_daemon_state():
            return {"error": "Stop the daemon first"}
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
            self.notifications.append({"message": "No button press detected", "error": True})
            return {"error": "timeout"}

        code, label = int(parts[1]), parts[2]
        device = parts[3] if len(parts) > 3 else "?"
        config = await self.get_config()
        config[f"trigger_{which}_code"] = code
        config[f"trigger_{which}_label"] = label
        await self.save_config(config)
        decky_plugin.logger.info(f"Detected {which} trigger: {label} ({code}) from {device}")
        self.notifications.append({"message": f"{which.capitalize()} trigger set to {label}"})
        return {"code": code, "label": label, "device": device}

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
        res = self.notifications.copy()
        self.notifications.clear()
        return res

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
        # Trigger/replay/hold settings are only read by the daemon at startup, so
        # the UI restricts editing them to when the daemon is stopped. We no longer
        # signal a running daemon here to reload (that behaviour was removed).
        try:
            with open(self.config_path, "w") as f:
                json.dump(config, f, indent=4)
            decky_plugin.logger.info("Config saved. Trigger/hold changes apply on next daemon start.")
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to save config to {self.config_path}: {e}\n{traceback.format_exc()}")
            return False

    async def get_slots_data(self) -> dict:
        return self._get_data()

    async def save_slots_data(self, data: dict) -> bool:
        try:
            with open(self.data_path, "w") as f:
                json.dump(data, f, indent=4)
            if await self.get_daemon_state():
                self.start_daemon()
            return True
        except Exception as e:
            decky_plugin.logger.error(f"Failed to save slots data to {self.data_path}: {e}\n{traceback.format_exc()}")
            return False

    async def get_daemon_state(self) -> bool:
        if Plugin.process is None: return False
        return Plugin.process.poll() is None

    async def set_daemon_state(self, is_running: bool) -> bool:
        if is_running:
            if not self.start_daemon():
                raise Exception("Failed to start Decky Macro daemon. Check logs for details.")
        else:
            self.stop_daemon()
        return True

    async def _main(self):
        # Daemon DOES NOT start by default anymore
        pass

    async def _unload(self):
        self.stop_daemon()

