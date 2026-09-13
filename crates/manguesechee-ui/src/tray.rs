//! Native Linux system tray implementation using the KDE / freedesktop
//! StatusNotifierItem D-Bus standard (ksni).
//!
//! Provides close-to-tray, minimize-to-tray, live status display, quick
//! cursor lock toggle, screen switching, and clean exit.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
use ksni::{Category, Icon, Status, ToolTip};
use manguesechee_core::ipc::{self, PeerInfo};
use slint::ComponentHandle;
use std::sync::OnceLock;
use tracing::{info, warn};

/// System tray item representing the Manguesechee KVM instance on desktop panels.
pub struct ManguesecheeTray {
    pub window: slint::Weak<crate::MainWindow>,
    pub status_text: String,
    pub is_forwarding: bool,
    pub is_service_running: bool,
    pub cursor_locked: bool,
    pub connected_to: Option<String>,
    pub peers: Vec<PeerInfo>,
}

impl ManguesecheeTray {
    pub fn new(window: slint::Weak<crate::MainWindow>) -> Self {
        Self {
            window,
            status_text: "Starting…".into(),
            is_forwarding: false,
            is_service_running: true,
            cursor_locked: false,
            connected_to: None,
            peers: Vec::new(),
        }
    }

    /// Spawns the StatusNotifierItem tray on a background thread.
    /// Returns the handle for live state updates, or None if SNI registration failed.
    pub fn spawn_tray(self) -> Option<Handle<Self>> {
        match self.spawn() {
            Ok(handle) => {
                info!("System tray (StatusNotifierItem) registered successfully");
                Some(handle)
            }
            Err(e) => {
                warn!("Could not initialize system tray (SNI unavailable): {e}");
                None
            }
        }
    }
}

impl ksni::Tray for ManguesecheeTray {
    fn id(&self) -> String {
        "manguesechee".into()
    }

    fn title(&self) -> String {
        "Manguesechee".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn status(&self) -> Status {
        Status::Active
    }

    fn icon_name(&self) -> String {
        if self.cursor_locked {
            "changes-prevent".into()
        } else if self.is_forwarding {
            "network-transmit-receive".into()
        } else {
            "input-keyboard".into()
        }
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        static ICON: OnceLock<Option<Icon>> = OnceLock::new();
        let icon_opt = ICON.get_or_init(|| {
            let img = image::load_from_memory_with_format(
                include_bytes!("../../../assets/icons/manguesechee-48.png"),
                image::ImageFormat::Png,
            ).ok()?;
            let width = img.width() as i32;
            let height = img.height() as i32;
            let mut data = img.into_rgba8().into_vec();
            // FreeDesktop SNI requires ARGB32 in network byte order
            for pixel in data.chunks_exact_mut(4) {
                pixel.rotate_right(1); // RGBA -> ARGB
            }
            Some(Icon {
                width,
                height,
                data,
            })
        });
        icon_opt.as_ref().cloned().into_iter().collect()
    }

    fn tool_tip(&self) -> ToolTip {
        let desc = if self.cursor_locked {
            format!("{} | 🔒 Cursor Locked", self.status_text)
        } else {
            self.status_text.clone()
        };
        ToolTip {
            title: "Manguesechee KVM".into(),
            description: desc,
            icon_name: self.icon_name(),
            icon_pixmap: self.icon_pixmap(),
        }
    }

    /// Left-click activation: restore and show the main window.
    fn activate(&mut self, _x: i32, _y: i32) {
        let window = self.window.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(w) = window.upgrade() {
                let _ = w.show();
                w.window().set_minimized(false);
            }
        });
    }

    /// Context menu (right-click).
    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = Vec::new();

        // 1. Show Manguesechee
        let window = self.window.clone();
        items.push(MenuItem::Standard(StandardItem {
            label: "Show Manguesechee".into(),
            activate: Box::new(move |_this: &mut Self| {
                let w_weak = window.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w_weak.upgrade() {
                        let _ = w.show();
                        w.window().set_minimized(false);
                    }
                });
            }),
            ..Default::default()
        }));

        items.push(MenuItem::Separator);

        // 2. Live Connection Status line (display only)
        let status_label = if !self.is_service_running {
            "Status: Service Stopped".into()
        } else if let Some(ref peer) = self.connected_to {
            let friendly = self.peers.iter()
                .find(|p| p.address == *peer || peer.contains(&p.address) || p.name == *peer)
                .map(|p| p.effective_display_name())
                .unwrap_or_else(|| manguesechee_core::names::clean_display_name("", peer));
            format!("Status: Forwarding → {friendly}")
        } else {
            format!("Status: {}", self.status_text)
        };
        items.push(MenuItem::Standard(StandardItem {
            label: status_label,
            enabled: false,
            ..Default::default()
        }));

        // 3. Connect & Disconnect Controls
        if self.connected_to.is_some() || self.is_forwarding {
            items.push(MenuItem::Standard(StandardItem {
                label: "Disconnect".into(),
                activate: Box::new(|_this: &mut Self| {
                    send_tray_ipc(ipc::GuiCommand::Disconnect);
                }),
                ..Default::default()
            }));
        } else if self.is_service_running && !self.peers.is_empty() {
            let mut connect_items = Vec::new();
            for peer in &self.peers {
                let addr = peer.address.clone();
                let name = peer.effective_display_name();
                connect_items.push(MenuItem::Standard(StandardItem {
                    label: format!("{name} ({addr})"),
                    activate: Box::new(move |_this: &mut Self| {
                        send_tray_ipc(ipc::GuiCommand::Connect { address: addr.clone() });
                    }),
                    ..Default::default()
                }));
            }
            items.push(MenuItem::SubMenu(SubMenu {
                label: "Connect to Peer".into(),
                submenu: connect_items,
                ..Default::default()
            }));
        }

        items.push(MenuItem::Separator);

        // 4. Cursor Lock Toggle (Checkmark)
        let locked = self.cursor_locked;
        let lock_label = if locked {
            "🔒 Cursor Locked (Local Screen)"
        } else {
            "🔓 Lock Cursor to Screen"
        };
        let w_lock = self.window.clone();
        items.push(MenuItem::Checkmark(CheckmarkItem {
            label: lock_label.into(),
            checked: locked,
            enabled: self.is_service_running,
            activate: Box::new(move |this: &mut Self| {
                let new_lock = !this.cursor_locked;
                this.cursor_locked = new_lock;
                send_tray_ipc(ipc::GuiCommand::SetCursorLock { locked: new_lock });
                if let Ok(mut cfg) = manguesechee_core::config::load() {
                    cfg.input.cursor_locked = new_lock;
                    let _ = manguesechee_core::config::save(&cfg);
                }
                let w_clone = w_lock.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(w) = w_clone.upgrade() {
                        w.set_cursor_locked(new_lock);
                        w.set_settings_feedback(if new_lock {
                            "🔒 Cursor locked to local screen".into()
                        } else {
                            "🔓 Cursor unlocked — edge switching enabled".into()
                        });
                    }
                });
            }),
            ..Default::default()
        }));

        // 5. Switch Screen Action
        items.push(MenuItem::Standard(StandardItem {
            label: "🖥️ Switch Screen (Hotkey)".into(),
            enabled: self.is_service_running,
            activate: Box::new(|_this: &mut Self| {
                send_tray_ipc(ipc::GuiCommand::SwitchScreen);
            }),
            ..Default::default()
        }));

        items.push(MenuItem::Separator);

        // 6. Service Controls (Start, Stop, Restart)
        if self.is_service_running {
            items.push(MenuItem::Standard(StandardItem {
                label: "🔄 Restart Daemon".into(),
                activate: Box::new(|_this: &mut Self| {
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "restart", "manguesechee-agent"])
                        .spawn();
                }),
                ..Default::default()
            }));
            items.push(MenuItem::Standard(StandardItem {
                label: "⏹ Stop Daemon".into(),
                activate: Box::new(|this: &mut Self| {
                    send_tray_ipc(ipc::GuiCommand::Shutdown);
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "stop", "manguesechee-agent"])
                        .spawn();
                    this.is_service_running = false;
                    this.status_text = "Service Stopped".into();
                }),
                ..Default::default()
            }));
        } else {
            items.push(MenuItem::Standard(StandardItem {
                label: "▶ Start Daemon".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = std::process::Command::new("systemctl")
                        .args(["--user", "start", "manguesechee-agent"])
                        .spawn();
                    this.is_service_running = true;
                    this.status_text = "Starting…".into();
                }),
                ..Default::default()
            }));
        }

        items.push(MenuItem::Separator);

        // 7. Quit Manguesechee
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit Manguesechee".into(),
            activate: Box::new(|_this: &mut Self| {
                info!("Quit requested from system tray");
                let _ = slint::invoke_from_event_loop(move || {
                    let _ = slint::quit_event_loop();
                });
            }),
            ..Default::default()
        }));

        items
    }
}

fn send_tray_ipc(cmd: ipc::GuiCommand) {
    use std::io::Write;
    if let Ok(mut s) = std::os::unix::net::UnixStream::connect(ipc::socket_path()) {
        if let Ok(j) = serde_json::to_string(&cmd) {
            let _ = s.write_all(format!("{j}\n").as_bytes());
        }
    }
}
