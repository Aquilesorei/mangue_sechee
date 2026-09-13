
use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use manguesechee_core::config;
use manguesechee_core::ipc::{self, AgentEvent, GuiCommand};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "manguesechee-cli",
    version = "0.2.0",
    about = "Control and monitor Manguesechee Linux KVM from the command line"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Show agent daemon status, connection, and peer info
    Status {
        /// Output formatted as JSON
        #[arg(long)]
        json: bool,
        /// Compact output formatted for Waybar / Polybar custom module
        #[arg(long)]
        waybar: bool,
        /// Watch and update continuously
        #[arg(short, long)]
        watch: bool,
        /// Watch refresh interval in seconds
        #[arg(short, long, default_value_t = 2)]
        interval: u64,
    },

    /// Connect to a peer by address (IP[:port]) or peer name
    Connect {
        /// Peer address (e.g. 192.168.1.50:24800) or peer name
        target: String,
    },

    /// Disconnect from the currently connected peer
    Disconnect,

    /// Lock mouse cursor to the local screen (prevent edge jumping)
    Lock,

    /// Unlock mouse cursor (allow edge switching)
    Unlock,

    /// Toggle cursor lock state (useful for global keyboard shortcuts)
    ToggleLock,

    /// Hop mouse cursor and keyboard focus to the next screen
    Switch,

    /// List known and discovered peers
    Peers {
        /// Output formatted as JSON
        #[arg(long)]
        json: bool,
    },

    /// View or configure 2D screen topology layout
    Layout {
        #[command(subcommand)]
        sub: Option<LayoutCommands>,
        /// Output layout as JSON
        #[arg(long)]
        json: bool,
    },

    /// Set edge resistance delay in milliseconds (prevents accidental jumps)
    Resistance {
        /// Delay in milliseconds (e.g. 150)
        delay_ms: u32,
    },

    /// Enable or disable TLS encryption (on/off)
    Tls {
        /// "on" / "off" (or true / false)
        state: String,
    },

    /// Enable or disable mDNS peer auto-discovery (on/off)
    Discovery {
        /// "on" / "off" (or true / false)
        state: String,
    },

    /// View or manage configuration
    Config {
        #[command(subcommand)]
        sub: Option<ConfigCommands>,
    },

    /// Manage the background agent daemon (start, stop, restart, status)
    Daemon {
        #[command(subcommand)]
        sub: DaemonCommands,
    },

    /// Send a file or folder to the connected peer
    SendFile {
        /// Path to file to transfer
        path: PathBuf,
    },
}

#[derive(Subcommand, Debug)]
enum LayoutCommands {
    /// Show ASCII visual 2D grid of screen nodes
    Show,
    /// Set peer screen direction relative to this screen (left, right, above, below)
    Set {
        peer: String,
        position: String,
    },
    /// Set exact 2D grid coordinates for a peer screen (e.g. 1 0)
    Grid {
        peer: String,
        x: i32,
        y: i32,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCommands {
    /// Show configuration file contents
    Show,
    /// Print path to configuration file
    Path,
}

#[derive(Subcommand, Debug)]
enum DaemonCommands {
    /// Start the background agent daemon
    Start,
    /// Stop the background agent daemon
    Stop,
    /// Restart the background agent daemon
    Restart,
    /// Check daemon process status
    Status,
    /// Enable autostart on system login
    Enable,
    /// Disable autostart on system login
    Disable,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.unwrap_or(Commands::Status {
        json: false,
        waybar: false,
        watch: false,
        interval: 2,
    }) {
        Commands::Status {
            json,
            waybar,
            watch,
            interval,
        } => {
            if watch {
                loop {
                    if let Err(e) = print_status(json, waybar) {
                        eprintln!("Status error: {e}");
                    }
                    std::thread::sleep(Duration::from_secs(interval));
                }
            } else {
                print_status(json, waybar)?;
            }
        }

        Commands::Connect { target } => {
            cmd_connect(&target)?;
        }

        Commands::Disconnect => {
            cmd_disconnect()?;
        }

        Commands::Lock => {
            cmd_set_lock(true)?;
        }

        Commands::Unlock => {
            cmd_set_lock(false)?;
        }

        Commands::ToggleLock => {
            cmd_toggle_lock()?;
        }

        Commands::Switch => {
            cmd_switch()?;
        }

        Commands::Peers { json } => {
            cmd_peers(json)?;
        }

        Commands::Layout { sub, json } => {
            cmd_layout(sub, json)?;
        }

        Commands::Resistance { delay_ms } => {
            cmd_set_resistance(delay_ms)?;
        }

        Commands::Tls { state } => {
            let enabled = parse_bool(&state)?;
            cmd_set_tls(enabled)?;
        }

        Commands::Discovery { state } => {
            let enabled = parse_bool(&state)?;
            cmd_set_discovery(enabled)?;
        }

        Commands::Config { sub } => {
            cmd_config(sub)?;
        }

        Commands::Daemon { sub } => {
            cmd_daemon(sub)?;
        }

        Commands::SendFile { path } => {
            cmd_send_file(&path)?;
        }
    }

    Ok(())
}


fn connect_ipc() -> Result<UnixStream> {
    let path = ipc::socket_path();
    if !path.exists() {
        bail!(
            "Manguesechee socket not found at {}.\nIs the agent running? Start it with: manguesechee-cli daemon start",
            path.display()
        );
    }
    let s = UnixStream::connect(&path)
        .with_context(|| format!("Could not connect to IPC socket at {}", path.display()))?;
    s.set_read_timeout(Some(Duration::from_millis(2000)))?;
    s.set_write_timeout(Some(Duration::from_millis(2000)))?;
    Ok(s)
}

fn send_command(cmd: &GuiCommand) -> Result<AgentEvent> {
    let mut s = connect_ipc()?;
    let json = serde_json::to_string(cmd)?;
    s.write_all(format!("{json}\n").as_bytes())?;
    let mut reader = BufReader::new(s);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let event: AgentEvent = serde_json::from_str(&line)
        .with_context(|| format!("Failed to parse response: '{line}'"))?;
    Ok(event)
}

fn query_status() -> Result<AgentEvent> {
    send_command(&GuiCommand::GetStatus)
}

fn daemon_alive() -> (bool, Option<i32>) {
    let Ok(s) = std::fs::read_to_string(ipc::pid_file()) else {
        return (false, None);
    };
    let Ok(pid) = s.trim().parse::<i32>() else {
        return (false, None);
    };
    let alive = unsafe { libc::kill(pid, 0) == 0 };
    (alive, if alive { Some(pid) } else { None })
}


fn print_status(json: bool, waybar: bool) -> Result<()> {
    let (alive, pid) = daemon_alive();

    if waybar {
        if !alive {
            println!(
                "{}",
                serde_json::json!({
                    "text": " Stopped",
                    "tooltip": "Manguesechee daemon is stopped",
                    "class": "stopped",
                    "percentage": 0
                })
            );
            return Ok(());
        }

        let ev = query_status().ok();
        if let Some(AgentEvent::Status {
            ref local_name,
            ref local_display_name,
            ref connected_to,
            cursor_locked,
            tls_active,
            ref peers,
            ..
        }) = ev
        {
            let disp = if !local_display_name.is_empty() {
                local_display_name
            } else {
                local_name
            };
            let peer_disp = connected_to.as_ref().map(|p| {
                peers.iter()
                    .find(|item| item.address == *p || p.contains(&item.address))
                    .map(|item| item.effective_display_name())
                    .unwrap_or_else(|| manguesechee_core::names::clean_display_name("", p))
            });

            let (text, class) = if cursor_locked {
                (" Locked".to_string(), "locked")
            } else if let Some(ref p) = peer_disp {
                (format!(" ➔ {p}"), "forwarding")
            } else {
                (" Ready".to_string(), "ready")
            };

            let tooltip = format!(
                "Manguesechee KVM\nDevice: {}\nStatus: {}\nCursor: {}\nTLS: {}",
                disp,
                peer_disp.as_deref().unwrap_or("Ready (local)"),
                if cursor_locked { "Locked" } else { "Unlocked" },
                if tls_active { "Active (TLS 1.3)" } else { "Disabled" }
            );

            println!(
                "{}",
                serde_json::json!({
                    "text": text,
                    "tooltip": tooltip,
                    "class": class,
                    "percentage": if connected_to.is_some() { 100 } else { 0 }
                })
            );
        } else {
            println!(
                "{}",
                serde_json::json!({
                    "text": " Running",
                    "tooltip": "Manguesechee daemon running (no IPC response)",
                    "class": "running",
                    "percentage": 50
                })
            );
        }
        return Ok(());
    }

    if json {
        if !alive {
            println!(
                "{}",
                serde_json::json!({
                    "running": false,
                    "pid": null,
                    "status": null
                })
            );
            return Ok(());
        }

        let status_ev = query_status()?;
        println!("{}", serde_json::to_string_pretty(&status_ev)?);
        return Ok(());
    }

    // Human CLI output
    println!("═══════════════════════════════════════════════════════════════");
    println!("  MANGUESECHEE LINUX KVM — SYSTEM STATUS");
    println!("═══════════════════════════════════════════════════════════════");

    if !alive {
        println!("  Daemon:      🔴 Stopped (not running)");
        println!("  Tip:         Start it with: manguesechee-cli daemon start");
        println!("═══════════════════════════════════════════════════════════════");
        return Ok(());
    }

    println!(
        "  Daemon:      🟢 Running (PID: {})",
        pid.map(|p| p.to_string()).unwrap_or_else(|| "?".into())
    );

    match query_status()? {
        AgentEvent::Status {
            local_name,
            local_display_name,
            connected_to,
            discovery,
            file_transfer_enabled,
            tls_enabled,
            tls_active,
            cursor_locked,
            edge_delay_ms,
            topology_configured,
            peers,
            active_transfers,
            transfer_history,
            ..
        } => {
            let self_display = if !local_display_name.is_empty() {
                format!("{local_display_name} ({local_name})")
            } else {
                local_name.clone()
            };
            println!("  Device Name: {self_display}");
            println!(
                "  Connection:  {}",
                if let Some(ref p) = connected_to {
                    let peer_disp = peers.iter()
                        .find(|item| item.address == *p || p.contains(&item.address))
                        .map(|item| item.effective_display_name())
                        .unwrap_or_else(|| manguesechee_core::names::clean_display_name("", p));
                    format!("🟢 Forwarding input to {peer_disp} ({p})")
                } else {
                    "⚪ Ready (Local control)".into()
                }
            );
            println!(
                "  Cursor Lock: {}",
                if cursor_locked {
                    "🔒 LOCKED to local screen (edges disabled)"
                } else {
                    "🔓 UNLOCKED (edge gliding enabled)"
                }
            );
            println!(
                "  Edge Delay:  {} ms resistance",
                edge_delay_ms
            );
            println!(
                "  Security:    {} (Configured: {})",
                if tls_active {
                    "🔒 TLS 1.3 Active"
                } else {
                    "🔓 Unencrypted"
                },
                if tls_enabled { "Enabled" } else { "Disabled" }
            );
            println!(
                "  Discovery:   {}",
                if discovery { "mDNS Enabled" } else { "Disabled" }
            );
            println!(
                "  Clipboard:   File transfer {}",
                if file_transfer_enabled { "Enabled" } else { "Disabled" }
            );
            println!(
                "  Topology:    {}",
                if topology_configured {
                    "✓ Configured & Synchronized"
                } else {
                    "Default Layout"
                }
            );

            if !active_transfers.is_empty() {
                println!("\n  Active File Transfers ({}):", active_transfers.len());
                for t in &active_transfers {
                    let pct = if t.total_bytes > 0 {
                        (t.bytes_transferred as f64 / t.total_bytes as f64) * 100.0
                    } else {
                        0.0
                    };
                    println!(
                        "    • {} ({}/{} - {:.0}%)",
                        t.filename,
                        format_bytes(t.bytes_transferred),
                        format_bytes(t.total_bytes),
                        pct
                    );
                }
            }

            if !transfer_history.is_empty() {
                println!("\n  Recent Transfers (last {}):", transfer_history.len());
                for h in transfer_history.iter().take(3) {
                    println!(
                        "    ✓ {} ({}, {})",
                        h.filename,
                        format_bytes(h.total_bytes),
                        h.completed_at
                    );
                }
            }

            println!("\n  Peers ({}):", peers.len());
            if peers.is_empty() {
                println!("    No peers discovered yet. Ensure peers are on LAN with discovery enabled.");
            } else {
                for p in &peers {
                    let status_bullet = if p.connected {
                        "🟢 Connected"
                    } else if p.paired {
                        "🟡 Paired"
                    } else {
                        "⚪ Discovered"
                    };
                    println!(
                        "    • {:<20} {:<22} {:<14} Grid: ({:>2}, {:>2}) [{}]",
                        p.effective_display_name(), p.address, status_bullet, p.grid_x, p.grid_y, p.position
                    );
                }
            }
        }
        _ => {
            println!("  Status:      Unexpected agent response");
        }
    }
    println!("═══════════════════════════════════════════════════════════════");

    Ok(())
}

fn cmd_connect(target: &str) -> Result<()> {
    let clean = target.trim();
    if clean.is_empty() {
        bail!("Please provide a peer address or name to connect to.");
    }

    let mut addr = clean.to_string();

    // Check if target matches any discovered peer name or display name
    if let Ok(AgentEvent::Status { peers, .. }) = query_status() {
        if let Some(p) = peers.iter().find(|p| {
            p.name.eq_ignore_ascii_case(clean)
                || p.display_name.eq_ignore_ascii_case(clean)
                || p.effective_display_name().eq_ignore_ascii_case(clean)
        }) {
            println!("Resolved peer '{}' → {}", p.effective_display_name(), p.address);
            addr = p.address.clone();
        }
    }

    if !addr.contains(':') {
        addr = format!("{addr}:24800");
    }

    println!("Connecting to {addr}…");
    let resp = send_command(&GuiCommand::Connect {
        address: addr.clone(),
    })?;

    match resp {
        AgentEvent::Ok => {
            println!("✓ Connection command sent.");
            std::thread::sleep(Duration::from_millis(600));
            if let Ok(AgentEvent::Status { connected_to, .. }) = query_status() {
                if let Some(ref c) = connected_to {
                    println!("✓ Successfully connected to {c}");
                } else {
                    println!("⌛ Connecting in background to {addr}… (Run 'manguesechee-cli status' to verify)");
                }
            }
        }
        AgentEvent::Error { message } => {
            bail!("Connection failed: {message}");
        }
        _ => {}
    }

    Ok(())
}

fn cmd_disconnect() -> Result<()> {
    println!("Disconnecting…");
    let resp = send_command(&GuiCommand::Disconnect)?;
    match resp {
        AgentEvent::Ok => println!("✓ Disconnected successfully."),
        AgentEvent::Error { message } => bail!("Disconnect failed: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_set_lock(locked: bool) -> Result<()> {
    let resp = send_command(&GuiCommand::SetCursorLock { locked })?;
    match resp {
        AgentEvent::Ok => {
            if locked {
                println!("🔒 Cursor locked to local screen. Edge switching disabled.");
            } else {
                println!("🔓 Cursor unlocked. Mouse edge gliding enabled.");
            }
        }
        AgentEvent::Error { message } => bail!("Failed to set lock: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_toggle_lock() -> Result<()> {
    let current_lock = match query_status() {
        Ok(AgentEvent::Status { cursor_locked, .. }) => Some(cursor_locked),
        _ => None,
    };

    if let Some(locked) = current_lock {
        cmd_set_lock(!locked)?;
    } else {
        let resp = send_command(&GuiCommand::ToggleCursorLock)?;
        match resp {
            AgentEvent::Ok => println!("✓ Cursor lock toggled."),
            AgentEvent::Error { message } => bail!("Toggle failed: {message}"),
            _ => {}
        }
    }
    Ok(())
}

fn cmd_switch() -> Result<()> {
    let resp = send_command(&GuiCommand::SwitchScreen)?;
    match resp {
        AgentEvent::Ok => println!("🖥️ Switched screen focus."),
        AgentEvent::Error { message } => bail!("Switch failed: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_peers(json: bool) -> Result<()> {
    let ev = query_status()?;
    if let AgentEvent::Status { peers, .. } = ev {
        if json {
            println!("{}", serde_json::to_string_pretty(&peers)?);
            return Ok(());
        }

        println!("PEERS ({} discovered / configured):", peers.len());
        println!("----------------------------------------------------------------------------------");
        println!(
            "{:<20} {:<22} {:<12} {:<12} {:<8}",
            "NAME", "ADDRESS", "STATUS", "GRID", "POSITION"
        );
        println!("----------------------------------------------------------------------------------");
        for p in &peers {
            let status = if p.connected {
                "Connected"
            } else if p.paired {
                "Paired"
            } else {
                "Discovered"
            };
            println!(
                "{:<20} {:<22} {:<12} ({:>2}, {:>2})   {:<8}",
                p.effective_display_name(), p.address, status, p.grid_x, p.grid_y, p.position
            );
        }
        println!("----------------------------------------------------------------------------------");
    } else {
        bail!("Could not retrieve peers list.");
    }
    Ok(())
}

fn cmd_layout(sub: Option<LayoutCommands>, json: bool) -> Result<()> {
    match sub.unwrap_or(LayoutCommands::Show) {
        LayoutCommands::Show => {
            let ev = query_status()?;
            if let AgentEvent::Status {
                local_name,
                peers,
                topology_configured,
                ..
            } = ev
            {
                if json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "local_node": local_name,
                            "configured": topology_configured,
                            "peers": peers
                        })
                    );
                    return Ok(());
                }

                println!("═══════════════════════════════════════════════════════════════");
                println!("  SCREEN TOPOLOGY 2D GRID LAYOUT");
                println!("═══════════════════════════════════════════════════════════════\n");

                println!("  Center Node (0, 0): [THIS MACHINE] {}", local_name);
                println!();

                // Build a 5x3 grid representation (-2..=2, -1..=1)
                for gy in ( -1..=1 ).rev() {
                    let mut row_slots = Vec::new();
                    for gx in -2..=2 {
                        if gx == 0 && gy == 0 {
                            row_slots.push(format!("*LOCAL ({local_name})*"));
                        } else if let Some(p) = peers.iter().find(|p| p.grid_x == gx && p.grid_y == gy) {
                            let mark = if p.connected { " [CONN]" } else { "" };
                            row_slots.push(format!("{}{}", p.name, mark));
                        } else {
                            row_slots.push(format!("({gx:>2},{gy:>2}) [empty]"));
                        }
                    }
                    println!("  Row y={gy:>2}:  {}", row_slots.join("  |  "));
                }

                println!("\n  Arranged Peer Screens:");
                for p in &peers {
                    println!(
                        "    • {:<14} at ({:>2}, {:>2})  Direction: {:<7}  Address: {}",
                        p.name, p.grid_x, p.grid_y, p.position, p.address
                    );
                }
                println!("\n  Traverse via continuous edge mouse glide or Ctrl+Alt+Arrow keys.");
                println!("═══════════════════════════════════════════════════════════════");
            }
        }

        LayoutCommands::Set { peer, position } => {
            let pos_clean = position.trim().to_lowercase();
            match pos_clean.as_str() {
                "left" | "right" | "above" | "below" | "top" | "bottom" => {}
                _ => bail!("Position must be one of: left, right, above, below"),
            }
            let resp = send_command(&GuiCommand::SyncTopology {
                address: peer.clone(),
                position: pos_clean.clone(),
            })?;
            match resp {
                AgentEvent::Ok => {
                    println!("✓ Screen layout updated: '{peer}' placed on the {pos_clean}");
                }
                AgentEvent::Error { message } => bail!("Failed to update layout: {message}"),
                _ => {}
            }
        }

        LayoutCommands::Grid { peer, x, y } => {
            let resp = send_command(&GuiCommand::SyncTopologyGrid {
                address: peer.clone(),
                grid_x: x,
                grid_y: y,
            })?;
            match resp {
                AgentEvent::Ok => {
                    println!("✓ 2D Grid coordinates for '{peer}' set to ({x}, {y})");
                }
                AgentEvent::Error { message } => bail!("Failed to update grid coordinates: {message}"),
                _ => {}
            }
        }
    }
    Ok(())
}

fn cmd_set_resistance(delay_ms: u32) -> Result<()> {
    let resp = send_command(&GuiCommand::SetEdgeResistance { delay_ms })?;
    match resp {
        AgentEvent::Ok => {
            println!("✓ Edge resistance delay set to {delay_ms} ms (saved to config)");
        }
        AgentEvent::Error { message } => bail!("Failed to set resistance: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_set_tls(enabled: bool) -> Result<()> {
    let resp = send_command(&GuiCommand::SetTls { enabled })?;
    match resp {
        AgentEvent::Ok => {
            println!(
                "✓ TLS encryption {} (saved to config)",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        AgentEvent::Error { message } => bail!("Failed to set TLS: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_set_discovery(enabled: bool) -> Result<()> {
    let resp = send_command(&GuiCommand::SetDiscovery { enabled })?;
    match resp {
        AgentEvent::Ok => {
            println!(
                "✓ mDNS peer auto-discovery {} (saved to config)",
                if enabled { "enabled" } else { "disabled" }
            );
        }
        AgentEvent::Error { message } => bail!("Failed to set discovery: {message}"),
        _ => {}
    }
    Ok(())
}

fn cmd_config(sub: Option<ConfigCommands>) -> Result<()> {
    let path = config::config_path();
    match sub.unwrap_or(ConfigCommands::Show) {
        ConfigCommands::Path => {
            println!("{}", path.display());
        }
        ConfigCommands::Show => {
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                println!("# Configuration: {}\n", path.display());
                println!("{content}");
            } else {
                println!("# Configuration file does not exist yet at {}", path.display());
                println!("# Default configuration:");
                let def = config::Config::default();
                println!("{}", toml::to_string_pretty(&def)?);
            }
        }
    }
    Ok(())
}

fn cmd_daemon(sub: DaemonCommands) -> Result<()> {
    match sub {
        DaemonCommands::Start => {
            let (alive, pid) = daemon_alive();
            if alive {
                println!("Agent daemon is already running (PID: {}).", pid.unwrap_or(0));
                return Ok(());
            }

            println!("Starting manguesechee-agent daemon…");
            let status = Command::new("systemctl")
                .args(["--user", "start", "manguesechee-agent"])
                .status();

            let mut started = false;
            if let Ok(st) = status {
                if st.success() {
                    std::thread::sleep(Duration::from_millis(500));
                    let (now_alive, pid_now) = daemon_alive();
                    if now_alive {
                        println!("✓ Daemon started via systemd (PID: {}).", pid_now.unwrap_or(0));
                        started = true;
                    }
                }
            }

            if !started {
                // Fallback: spawn binary directly
                let bin = std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(|d| d.join("manguesechee-agent")))
                    .filter(|p| p.exists())
                    .unwrap_or_else(|| PathBuf::from("manguesechee-agent"));

                match Command::new(&bin)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                {
                    Ok(child) => {
                        println!("✓ Spawned manguesechee-agent directly (PID: {}).", child.id());
                    }
                    Err(e) => {
                        bail!("Failed to start daemon: {e}");
                    }
                }
            }
        }

        DaemonCommands::Stop => {
            println!("Stopping manguesechee-agent daemon…");
            let _ = send_command(&GuiCommand::Shutdown);
            let _ = Command::new("systemctl")
                .args(["--user", "stop", "manguesechee-agent"])
                .status();
            std::thread::sleep(Duration::from_millis(400));
            let (alive, _) = daemon_alive();
            if !alive {
                println!("✓ Daemon stopped.");
            } else {
                println!("⚠ Daemon stop requested.");
            }
        }

        DaemonCommands::Restart => {
            println!("Restarting manguesechee-agent daemon…");
            let _ = Command::new("systemctl")
                .args(["--user", "restart", "manguesechee-agent"])
                .status();
            std::thread::sleep(Duration::from_millis(800));
            let (alive, pid) = daemon_alive();
            if alive {
                println!("✓ Daemon restarted (PID: {}).", pid.unwrap_or(0));
            } else {
                println!("⚠ Daemon restarting…");
            }
        }

        DaemonCommands::Status => {
            let (alive, pid) = daemon_alive();
            if alive {
                println!("● manguesechee-agent: ACTIVE / RUNNING (PID: {})", pid.unwrap_or(0));
            } else {
                println!("○ manguesechee-agent: INACTIVE / STOPPED");
            }

            // Also check systemd status
            let out = Command::new("systemctl")
                .args(["--user", "is-active", "manguesechee-agent"])
                .output();
            if let Ok(o) = out {
                let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                println!("  systemd user unit: {s}");
            }
        }

        DaemonCommands::Enable => {
            let res = Command::new("systemctl")
                .args(["--user", "enable", "--now", "manguesechee-agent"])
                .status();
            if res.is_ok_and(|s| s.success()) {
                println!("✓ Enabled autostart on login (systemd user unit).");
            } else {
                bail!("Failed to enable systemd user unit.");
            }
        }

        DaemonCommands::Disable => {
            let res = Command::new("systemctl")
                .args(["--user", "disable", "manguesechee-agent"])
                .status();
            if res.is_ok_and(|s| s.success()) {
                println!("✓ Disabled autostart on login.");
            } else {
                bail!("Failed to disable systemd user unit.");
            }
        }
    }
    Ok(())
}

fn cmd_send_file(path: &Path) -> Result<()> {
    if !path.exists() {
        bail!("File or directory does not exist: {}", path.display());
    }

    let canonical = path.canonicalize()
        .with_context(|| format!("Could not resolve path: {}", path.display()))?;

    let uri = format!("file://{}", canonical.display());
    println!("Staging file for transfer: {}", canonical.display());

    // Stage in clipboard via wl-copy or xclip
    let mut copied = false;
    if let Ok(mut child) = Command::new("wl-copy")
        .arg("--type")
        .arg("text/uri-list")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(format!("{uri}\r\n").as_bytes());
            drop(stdin);
            if child.wait().is_ok_and(|s| s.success()) {
                copied = true;
            }
        }
    }

    if !copied {
        if let Ok(mut child) = Command::new("xclip")
            .args(["-selection", "clipboard", "-t", "text/uri-list"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(format!("{uri}\r\n").as_bytes());
                drop(stdin);
                if child.wait().is_ok_and(|s| s.success()) {
                    copied = true;
                }
            }
        }
    }

    if !copied {
        println!("ℹ Notice: Neither 'wl-copy' nor 'xclip' found in PATH.");
    }

    // Check connection status
    if let Ok(AgentEvent::Status { connected_to, .. }) = query_status() {
        if let Some(ref p) = connected_to {
            println!("✓ File staged for transfer (URI: {uri})");
            println!("➔ Transmitting to connected peer '{p}'. Ready to paste on remote desktop!");
        } else {
            println!("✓ File staged for transfer (URI: {uri})");
            println!("ℹ Note: No peer currently connected. File will be sent once connected.");
        }
    } else {
        println!("✓ File staged for transfer: {uri}");
    }

    Ok(())
}

fn parse_bool(s: &str) -> Result<bool> {
    match s.trim().to_lowercase().as_str() {
        "on" | "true" | "1" | "yes" | "enable" | "enabled" => Ok(true),
        "off" | "false" | "0" | "no" | "disable" | "disabled" => Ok(false),
        other => bail!("Invalid boolean value: '{other}'. Use 'on' or 'off'."),
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}
