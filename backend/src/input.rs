//! All input reading on ONE thread, driven by epoll:
//!  - every evdev device in /dev/input (gamepads, keyboards, ...),
//!  - the Steam Deck controller's raw HID reports (/dev/hidraw*): back buttons
//!    always, D-pad/A/B/left stick only while the macro picker is open,
//!  - inotify on /dev/input and /dev, so new or re-created devices (a game
//!    start, a resume from sleep) are picked up at once; a rescan every 10 s
//!    stays as a safety net.
//! Events are sent to the main loop as `Msg`s, with the device path shared
//! through an `Arc` (no allocation per event).

use crate::{
    find_deck_hidraw, nav_changes, paddle_changes, stick_nav, AxisRange, Msg, ANALOG_AXES,
    DECK_PADDLES, DECK_PADDLES_PATH,
};
use evdev::{Device, InputEvent, InputEventKind, Key};
use log::{info, warn};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

const RESCAN_MS: i32 = 10_000;

enum Source {
    Evdev {
        path: Arc<PathBuf>,
        dev: Device,
        analog: HashSet<u16>,
    },
    Hidraw {
        path: PathBuf,
        file: fs::File,
        paddles: [bool; 4],
        nav: [bool; 4],
        stick: i32,
    },
    Inotify,
}

struct Reader {
    epoll: i32,
    sources: HashMap<i32, Source>,
    evdev_paths: HashSet<PathBuf>,
    hidraw_paths: HashSet<PathBuf>,
    tx: Sender<Msg>,
    picker_open: Arc<AtomicBool>,
    hidraw_nodes: Arc<AtomicUsize>,
    paddle_path: Arc<PathBuf>,
}

fn set_nonblocking(fd: i32) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
}

impl Reader {
    fn watch(&mut self, fd: i32, source: Source) {
        let mut ev = libc::epoll_event { events: libc::EPOLLIN as u32, u64: fd as u64 };
        if unsafe { libc::epoll_ctl(self.epoll, libc::EPOLL_CTL_ADD, fd, &mut ev) } == 0 {
            self.sources.insert(fd, source);
        } else {
            warn!("epoll_ctl failed for fd {}", fd);
        }
    }

    fn unwatch(&mut self, fd: i32) -> Option<Source> {
        unsafe { libc::epoll_ctl(self.epoll, libc::EPOLL_CTL_DEL, fd, std::ptr::null_mut()) };
        self.sources.remove(&fd)
    }

    fn scan(&mut self) {
        if let Ok(dir) = fs::read_dir("/dev/input") {
            for entry in dir.flatten() {
                let path = entry.path();
                if path.to_string_lossy().contains("event") && !self.evdev_paths.contains(&path) {
                    self.open_evdev(path);
                }
            }
        }
        for path in find_deck_hidraw() {
            if !self.hidraw_paths.contains(&path) {
                self.open_hidraw(path);
            }
        }
    }

    fn open_evdev(&mut self, path: PathBuf) {
        let dev = match Device::open(&path) {
            Ok(d) => d,
            Err(_) => return,
        };
        // Analog axes are only taken from gamepads: motion sensors and
        // touchscreens also report ABS_X/ABS_Y.
        let is_gamepad = dev.supported_keys().map_or(false, |k| k.contains(Key::BTN_SOUTH));
        let mut analog = HashSet::new();
        let mut ranges = Vec::new();
        if is_gamepad {
            if let (Some(axes), Ok(state)) = (dev.supported_absolute_axes(), dev.get_abs_state()) {
                for code in ANALOG_AXES {
                    if axes.contains(evdev::AbsoluteAxisType(code)) {
                        let info = state[code as usize];
                        let range = if info.maximum > info.minimum {
                            AxisRange { min: info.minimum, max: info.maximum }
                        } else {
                            AxisRange::fallback(code)
                        };
                        analog.insert(code);
                        ranges.push((code, range));
                    }
                }
            }
            info!("Gamepad {:?} ({}): analog axes {:?}", path, dev.name().unwrap_or("?"), ranges);
        }
        let keys: Vec<u16> = dev
            .supported_keys()
            .map(|k| k.iter().map(|key| key.code()).collect())
            .unwrap_or_default();
        let _ = self.tx.send(Msg::Device {
            path: path.clone(),
            name: dev.name().unwrap_or("?").to_string(),
            is_gamepad,
            ranges,
            keys,
        });
        let fd = dev.as_raw_fd();
        set_nonblocking(fd);
        self.evdev_paths.insert(path.clone());
        self.watch(fd, Source::Evdev { path: Arc::new(path), dev, analog });
    }

    fn open_hidraw(&mut self, path: PathBuf) {
        let file = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warn!("Cannot open {:?} for back buttons: {}", path, e);
                return;
            }
        };
        let fd = file.as_raw_fd();
        set_nonblocking(fd);
        self.hidraw_paths.insert(path.clone());
        let before = self.hidraw_nodes.fetch_add(1, Ordering::Relaxed);
        info!("Reading Steam Deck controller reports from {:?}", path);
        if before == 0 {
            let _ = self.tx.send(Msg::Device {
                path: PathBuf::from(DECK_PADDLES_PATH),
                name: "Steam Deck back buttons".to_string(),
                is_gamepad: false,
                ranges: Vec::new(),
                keys: DECK_PADDLES.iter().map(|(c, _, _)| *c).collect(),
            });
        }
        self.watch(fd, Source::Hidraw { path, file, paddles: [false; 4], nav: [false; 4], stick: 0 });
    }

    fn drop_source(&mut self, fd: i32) {
        match self.unwatch(fd) {
            Some(Source::Evdev { path, .. }) => {
                info!("Input device {:?} gone", path);
                self.evdev_paths.remove(&*path);
                let _ = self.tx.send(Msg::Removed((*path).clone()));
            }
            Some(Source::Hidraw { path, .. }) => {
                info!("Steam Deck controller node {:?} gone", path);
                self.hidraw_paths.remove(&path);
                if self.hidraw_nodes.fetch_sub(1, Ordering::Relaxed) == 1 {
                    let _ = self.tx.send(Msg::Removed(PathBuf::from(DECK_PADDLES_PATH)));
                }
            }
            _ => {}
        }
    }

    /// Reads what a ready fd has; returns false if the source is gone.
    fn read_ready(&mut self, fd: i32) -> bool {
        let picker_open = self.picker_open.load(Ordering::Relaxed);
        let tx = self.tx.clone();
        let paddle_path = Arc::clone(&self.paddle_path);
        match self.sources.get_mut(&fd) {
            Some(Source::Evdev { path, dev, analog }) => match dev.fetch_events() {
                Ok(events) => {
                    for event in events {
                        let kind = event.kind();
                        let keep = match kind {
                            InputEventKind::Key(_) => true,
                            // D-pad as hat (Steam's virtual pad) + analog axes of gamepads
                            InputEventKind::AbsAxis(a) => a.0 == 0x10 || a.0 == 0x11 || analog.contains(&a.0),
                            _ => false,
                        };
                        if keep {
                            let _ = tx.send(Msg::Input(Arc::clone(path), event));
                        }
                    }
                    true
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
                Err(_) => false,
            },
            Some(Source::Hidraw { file, paddles, nav, stick, .. }) => {
                let mut buf = [0u8; 64];
                loop {
                    match file.read(&mut buf) {
                        Ok(n) if n > 0 => {
                            let report = &buf[..n];
                            for (code, pressed) in paddle_changes(report, paddles) {
                                let ev = InputEvent::new(evdev::EventType::KEY, code, pressed as i32);
                                let _ = tx.send(Msg::Input(Arc::clone(&paddle_path), ev));
                            }
                            // Track nav state always (no spurious press when the picker
                            // opens with a button held), only send it while open.
                            let mut moves = nav_changes(report, nav);
                            moves.extend(stick_nav(report, stick));
                            if picker_open {
                                for (code, pressed) in moves {
                                    let ev = InputEvent::new(evdev::EventType::KEY, code, pressed as i32);
                                    let _ = tx.send(Msg::Input(Arc::clone(&paddle_path), ev));
                                }
                            }
                        }
                        Ok(_) => return false,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return true,
                        Err(_) => return false,
                    }
                }
            }
            Some(Source::Inotify) => {
                let mut buf = [0u8; 4096];
                unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
                // Let udev finish setting up the new node before opening it.
                thread::sleep(Duration::from_millis(150));
                self.scan();
                true
            }
            None => true,
        }
    }
}

/// Starts the input thread. `hidraw_nodes` counts open Deck controller nodes
/// (0 = no raw back buttons / picker navigation available).
pub fn spawn_input_thread(tx: Sender<Msg>, picker_open: Arc<AtomicBool>, hidraw_nodes: Arc<AtomicUsize>) {
    thread::spawn(move || {
        let epoll = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if epoll < 0 {
            warn!("epoll unavailable: no input can be read");
            return;
        }
        let mut reader = Reader {
            epoll,
            sources: HashMap::new(),
            evdev_paths: HashSet::new(),
            hidraw_paths: HashSet::new(),
            tx,
            picker_open,
            hidraw_nodes,
            paddle_path: Arc::new(PathBuf::from(DECK_PADDLES_PATH)),
        };

        let ifd = unsafe { libc::inotify_init1(libc::IN_CLOEXEC | libc::IN_NONBLOCK) };
        if ifd >= 0 {
            for dir in ["/dev/input", "/dev"] {
                let c = std::ffi::CString::new(dir).unwrap();
                unsafe { libc::inotify_add_watch(ifd, c.as_ptr(), libc::IN_CREATE | libc::IN_ATTRIB) };
            }
            reader.watch(ifd, Source::Inotify);
        } else {
            warn!("inotify unavailable, rescanning devices every 10 s only");
        }

        reader.scan();
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 32];
        loop {
            let n = unsafe { libc::epoll_wait(epoll, events.as_mut_ptr(), events.len() as i32, RESCAN_MS) };
            if n < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                warn!("epoll_wait failed: {}", std::io::Error::last_os_error());
                thread::sleep(Duration::from_secs(1));
                continue;
            }
            if n == 0 {
                reader.scan();
                continue;
            }
            for i in 0..n as usize {
                let fd = events.get(i).map(|e| e.u64 as i32).unwrap_or(-1);
                let flags = events.get(i).map(|e| e.events).unwrap_or(0);
                let alive = reader.read_ready(fd);
                if !alive || flags & (libc::EPOLLHUP | libc::EPOLLERR) as u32 != 0 && !matches!(reader.sources.get(&fd), Some(Source::Inotify)) {
                    reader.drop_source(fd);
                }
            }
        }
    });
}
