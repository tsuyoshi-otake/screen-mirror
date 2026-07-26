use anyhow::{Context, Result};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

use crate::autostart;
use crate::config::{AppConfig, StartupMode};
use crate::pipeline::{self, PipelineHandle};

const ID_START_SENDER: &str = "start-sender";
const ID_START_RECEIVER: &str = "start-receiver";
const ID_STOP: &str = "stop";
const ID_AUTOSTART: &str = "autostart";
const ID_CHECK_UPDATE: &str = "check-update";
const ID_RUN_DIAGNOSTICS: &str = "run-diagnostics";
const ID_RUN_PEER_DIAGNOSTICS: &str = "run-peer-diagnostics";
const ID_INSTALL_VDD: &str = "install-vdd";
const ID_LIST_VDD: &str = "list-vdd";
const ID_ENABLE_VDD: &str = "enable-vdd";
const ID_DISABLE_VDD: &str = "disable-vdd";
const ID_REMOVE_VDD: &str = "remove-vdd";
const ID_OPEN_DISPLAY_SETTINGS: &str = "open-display-settings";
const ID_OPEN_VDD: &str = "open-vdd";
const ID_OPEN_CONFIG: &str = "open-config";
const ID_RELOAD_CONFIG: &str = "reload-config";
const ID_QUIT: &str = "quit";

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ActiveMode {
    Idle,
    Sender,
    Receiver,
}

pub fn run() -> Result<()> {
    crate::logging::append("tray run start");
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let mut app = TrayApp::new()?;
    crate::logging::append(format!(
        "config loaded: startup_mode={:?} autostart={}",
        app.config.startup_mode, app.config.autostart
    ));
    app.initialize_tray()?;
    crate::logging::append("tray initialized");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + Duration::from_millis(500));

        match event {
            Event::UserEvent(UserEvent::Menu(event)) => {
                app.handle_menu(event.id().as_ref(), control_flow);
            }
            Event::LoopDestroyed => {
                let _ = app.stop_current();
            }
            _ => {}
        }

        app.poll_update_status();
        app.reap_finished_pipeline();
    });
}

struct TrayApp {
    config: AppConfig,
    config_path: std::path::PathBuf,
    active_mode: ActiveMode,
    pipeline: Option<PipelineHandle>,
    sender_supervisor: Option<crate::lan::SenderSupervisor>,
    control_server: Option<crate::control::ControlServer>,
    diagnostics_server: Option<crate::diagnostics::DiagnosticsServer>,
    announcer: Option<crate::lan::Announcer>,
    sleep_guard: Option<crate::power::SleepGuard>,
    render_window: Option<crate::receiver_window::RenderWindowGuard>,
    update_status_tx: Sender<String>,
    update_status_rx: Receiver<String>,
    tray: Option<TrayIcon>,
    items: Option<TrayItems>,
}

struct TrayItems {
    status: MenuItem,
    start_sender: MenuItem,
    start_receiver: MenuItem,
    stop: MenuItem,
    autostart: MenuItem,
}

impl TrayApp {
    fn new() -> Result<Self> {
        let (mut config, config_path) = AppConfig::load_or_create()?;
        if let Ok(enabled) = autostart::is_enabled() {
            config.autostart = enabled;
        }
        let (update_status_tx, update_status_rx) = mpsc::channel();

        Ok(Self {
            config,
            config_path,
            active_mode: ActiveMode::Idle,
            pipeline: None,
            sender_supervisor: None,
            control_server: None,
            diagnostics_server: None,
            announcer: None,
            sleep_guard: None,
            render_window: None,
            update_status_tx,
            update_status_rx,
            tray: None,
            items: None,
        })
    }

    fn initialize_tray(&mut self) -> Result<()> {
        crate::logging::append("initialize_tray");
        if self.tray.is_some() {
            return Ok(());
        }

        let items = TrayItems::new(self.config.autostart)?;
        let menu = Menu::new();
        let sep1 = PredefinedMenuItem::separator();
        let sep2 = PredefinedMenuItem::separator();
        let sep3 = PredefinedMenuItem::separator();
        let sep4 = PredefinedMenuItem::separator();
        let check_update = MenuItem::with_id(ID_CHECK_UPDATE, "Check for Updates", true, None);
        let run_diagnostics = MenuItem::with_id(ID_RUN_DIAGNOSTICS, "Run Diagnostics", true, None);
        let run_peer_diagnostics =
            MenuItem::with_id(ID_RUN_PEER_DIAGNOSTICS, "Run Peer Diagnostics", true, None);
        let install_vdd = MenuItem::with_id(
            ID_INSTALL_VDD,
            "Install/Repair Virtual Display Driver",
            true,
            None,
        );
        let list_vdd = MenuItem::with_id(ID_LIST_VDD, "Show Virtual Display Status", true, None);
        let enable_vdd =
            MenuItem::with_id(ID_ENABLE_VDD, "Enable Virtual Display Driver", true, None);
        let disable_vdd =
            MenuItem::with_id(ID_DISABLE_VDD, "Disable Virtual Display Driver", true, None);
        let remove_vdd = MenuItem::with_id(
            ID_REMOVE_VDD,
            "Remove All Bundled Virtual Displays",
            true,
            None,
        );
        let open_display_settings = MenuItem::with_id(
            ID_OPEN_DISPLAY_SETTINGS,
            "Open Display Settings",
            true,
            None,
        );
        let open_vdd =
            MenuItem::with_id(ID_OPEN_VDD, "Open Virtual Display Driver Page", true, None);
        let open_config = MenuItem::with_id(ID_OPEN_CONFIG, "Open Config", true, None);
        let reload_config = MenuItem::with_id(ID_RELOAD_CONFIG, "Reload Config", true, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit", true, None);

        menu.append(&items.status)?;
        menu.append(&sep1)?;
        menu.append(&items.start_sender)?;
        menu.append(&items.start_receiver)?;
        menu.append(&items.stop)?;
        menu.append(&sep2)?;
        menu.append(&items.autostart)?;
        menu.append(&check_update)?;
        menu.append(&run_diagnostics)?;
        menu.append(&run_peer_diagnostics)?;
        menu.append(&sep3)?;
        menu.append(&install_vdd)?;
        menu.append(&list_vdd)?;
        menu.append(&enable_vdd)?;
        menu.append(&disable_vdd)?;
        menu.append(&remove_vdd)?;
        menu.append(&open_display_settings)?;
        menu.append(&open_vdd)?;
        menu.append(&open_config)?;
        menu.append(&reload_config)?;
        menu.append(&sep4)?;
        menu.append(&quit)?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip(app_tooltip())
            .with_icon(app_icon()?)
            .with_menu_on_left_click(false)
            .with_menu_on_right_click(true)
            .build()
            .context("failed to create tray icon")?;

        self.items = Some(items);
        self.tray = Some(tray);
        self.sync_menu();
        crate::updater::start_background_update_checks();
        self.restart_diagnostics_server();

        if let Err(error) = autostart::set_enabled(self.config.autostart) {
            self.set_error(format!("Autostart update failed: {error:#}"));
        }

        match self.config.startup_mode {
            StartupMode::Idle => crate::logging::append("startup mode idle"),
            StartupMode::Sender => {
                crate::logging::append("startup mode sender");
                self.start_sender()
            }
            StartupMode::Receiver => {
                crate::logging::append("startup mode receiver");
                self.start_receiver()
            }
        }

        Ok(())
    }

    fn handle_menu(&mut self, id: &str, control_flow: &mut ControlFlow) {
        match id {
            ID_START_SENDER => self.start_sender(),
            ID_START_RECEIVER => self.start_receiver(),
            ID_STOP => {
                if let Err(error) = self.stop_current() {
                    self.set_error(format!("Stop failed: {error:#}"));
                }
                self.config.startup_mode = StartupMode::Idle;
                self.save_config();
            }
            ID_AUTOSTART => self.toggle_autostart(),
            ID_CHECK_UPDATE => self.check_for_updates(),
            ID_RUN_DIAGNOSTICS => self.run_diagnostics(),
            ID_RUN_PEER_DIAGNOSTICS => self.run_peer_diagnostics(),
            ID_INSTALL_VDD => self.run_vdd_action("Install"),
            ID_LIST_VDD => self.run_vdd_action("List"),
            ID_ENABLE_VDD => self.run_vdd_action("Enable"),
            ID_DISABLE_VDD => self.run_vdd_action("Disable"),
            ID_REMOVE_VDD => self.run_vdd_action("Remove"),
            ID_OPEN_DISPLAY_SETTINGS => self.open_display_settings(),
            ID_OPEN_VDD => self.open_vdd_page(),
            ID_OPEN_CONFIG => self.open_config(),
            ID_RELOAD_CONFIG => self.reload_config(),
            ID_QUIT => {
                let _ = self.stop_current();
                if let Some(server) = self.diagnostics_server.take() {
                    server.stop();
                }
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    }

    fn start_sender(&mut self) {
        if let Err(error) = self.reload_config_from_disk() {
            self.set_error(format!("Config reload failed: {error:#}"));
            return;
        }

        if let Err(error) = self.stop_current() {
            self.set_error(format!("Stop failed: {error:#}"));
            return;
        }

        let args = self.config.send_args();
        match crate::control::ControlServer::start(&self.config.security.pin) {
            Ok(server) => self.control_server = Some(server),
            Err(error) => eprintln!("touch control server failed: {error:#}"),
        }
        let audio_port = self
            .config
            .send
            .audio_enabled
            .then_some(self.config.send.audio_port);
        match crate::lan::Announcer::sender(
            self.config.send.port,
            audio_port,
            &self.config.security.pin,
        ) {
            Ok(announcer) => self.announcer = Some(announcer),
            Err(error) => eprintln!("sender discovery announce failed: {error:#}"),
        }

        if crate::lan::wants_auto_host(&args.host) {
            self.sender_supervisor = Some(crate::lan::SenderSupervisor::start(args));
            self.active_mode = ActiveMode::Sender;
            self.config.startup_mode = StartupMode::Sender;
            self.save_config();
            self.sync_menu();
            return;
        }

        let args = match crate::lan::resolve_sender_args(args) {
            Ok(args) => args,
            Err(error) => {
                self.set_error(format!("Receiver discovery failed: {error:#}"));
                return;
            }
        };
        match pipeline::build_sender_pipeline(&args) {
            Ok(description) => {
                eprintln!("sender pipeline: {description}");
                self.pipeline = Some(pipeline::spawn_pipeline(description));
                self.active_mode = ActiveMode::Sender;
                self.config.startup_mode = StartupMode::Sender;
                self.save_config();
                self.sync_menu();
            }
            Err(error) => self.set_error(format!("Sender start failed: {error:#}")),
        }
    }

    fn start_receiver(&mut self) {
        crate::logging::append("start_receiver requested");
        if let Err(error) = self.reload_config_from_disk() {
            self.set_error(format!("Config reload failed: {error:#}"));
            return;
        }

        if let Err(error) = self.stop_current() {
            self.set_error(format!("Stop failed: {error:#}"));
            return;
        }

        let args = self.config.recv_args();
        match pipeline::build_receiver_pipeline(&args) {
            Ok(description) => {
                crate::logging::append(format!("receiver pipeline: {description}"));
                eprintln!("receiver pipeline: {description}");
                self.pipeline = Some(pipeline::spawn_pipeline(description));
                self.sleep_guard = Some(crate::power::SleepGuard::receiver());
                self.render_window = Some(crate::receiver_window::RenderWindowGuard::start());
                let audio_port = self
                    .config
                    .recv
                    .audio_enabled
                    .then_some(self.config.recv.audio_port);
                match crate::lan::Announcer::receiver(
                    self.config.recv.port,
                    audio_port,
                    &self.config.security.pin,
                ) {
                    Ok(announcer) => self.announcer = Some(announcer),
                    Err(error) => eprintln!("receiver discovery announce failed: {error:#}"),
                }
                self.active_mode = ActiveMode::Receiver;
                self.config.startup_mode = StartupMode::Receiver;
                self.save_config();
                self.sync_menu();
            }
            Err(error) => self.set_error(format!("Receiver start failed: {error:#}")),
        }
    }

    fn stop_current(&mut self) -> Result<()> {
        if let Some(handle) = self.pipeline.take() {
            handle.stop()?;
        }
        if let Some(supervisor) = self.sender_supervisor.take() {
            supervisor.stop();
        }
        if let Some(server) = self.control_server.take() {
            server.stop();
        }
        if let Some(announcer) = self.announcer.take() {
            announcer.stop();
        }
        self.render_window = None;
        self.sleep_guard = None;
        self.active_mode = ActiveMode::Idle;
        self.sync_menu();
        Ok(())
    }

    fn reap_finished_pipeline(&mut self) {
        let Some(handle) = self.pipeline.as_ref() else {
            return;
        };
        if !handle.is_finished() {
            return;
        }

        let result = self
            .pipeline
            .take()
            .map(PipelineHandle::finish)
            .unwrap_or(Ok(()));
        if let Err(error) = result {
            self.set_error(format!("Pipeline stopped: {error:#}"));
        } else {
            crate::logging::append("pipeline stopped; returning to idle");
        }

        if let Some(supervisor) = self.sender_supervisor.take() {
            supervisor.stop();
        }
        if let Some(server) = self.control_server.take() {
            server.stop();
        }
        if let Some(announcer) = self.announcer.take() {
            announcer.stop();
        }
        self.render_window = None;
        self.sleep_guard = None;
        self.active_mode = ActiveMode::Idle;
        self.config.startup_mode = StartupMode::Idle;
        self.save_config();
        self.sync_menu();
    }

    fn toggle_autostart(&mut self) {
        let next = !self.config.autostart;
        match autostart::set_enabled(next) {
            Ok(()) => {
                self.config.autostart = next;
                self.save_config();
                self.sync_menu();
            }
            Err(error) => self.set_error(format!("Autostart update failed: {error:#}")),
        }
    }

    fn check_for_updates(&self) {
        crate::updater::start_manual_update_check(self.update_status_tx.clone());
        if let Some(items) = self.items.as_ref() {
            items.status.set_text("Status: update check started");
        }
    }

    fn run_diagnostics(&self) {
        let Some(script) = std::env::current_exe().ok().and_then(|path| {
            path.parent()
                .map(|parent| parent.join("diagnose-screen-mirror.ps1"))
        }) else {
            self.set_error("Failed to resolve diagnostics script path".to_string());
            return;
        };

        if !script.exists() {
            self.set_error(format!(
                "Diagnostics script not found: {}",
                script.display()
            ));
            return;
        }

        if let Err(error) = crate::process::hidden_command("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-File",
            ])
            .arg(script)
            .spawn()
        {
            self.set_error(format!("Failed to start diagnostics: {error}"));
        } else if let Some(items) = self.items.as_ref() {
            items
                .status
                .set_text("Status: diagnostics copied to clipboard");
        }
    }

    fn run_peer_diagnostics(&self) {
        let pin = self.config.security.pin.clone();
        std::thread::spawn(move || {
            let result = (|| -> Result<()> {
                let peers = crate::lan::discover_senders_with_pin(Duration::from_secs(3), &pin)?;
                let peer = peers
                    .first()
                    .ok_or_else(|| anyhow::anyhow!("no sender found with matching PIN"))?;
                let port = peer
                    .announcement
                    .diagnostics_port
                    .unwrap_or(sm_core::diagnostics::DIAGNOSTICS_PORT);
                let report = crate::diagnostics::request_remote_report(peer.address, port, &pin)?;
                let path = crate::diagnostics::save_report_to_clipboard_and_notepad(
                    &report,
                    &peer.announcement.device_name,
                )?;
                crate::logging::append(format!(
                    "peer diagnostics copied to clipboard: {}",
                    path.display()
                ));
                Ok(())
            })();

            if let Err(error) = result {
                crate::logging::append(format!("peer diagnostics failed: {error:#}"));
            }
        });

        if let Some(items) = self.items.as_ref() {
            items.status.set_text("Status: peer diagnostics requested");
        }
    }

    fn open_config(&self) {
        if let Err(error) = std::process::Command::new("notepad.exe")
            .arg(&self.config_path)
            .spawn()
        {
            eprintln!("failed to open config: {error}");
        }
    }

    fn run_vdd_action(&self, action: &str) {
        let Some(script) = std::env::current_exe().ok().and_then(|path| {
            path.parent()
                .map(|parent| parent.join("install-bundled-vdd.ps1"))
        }) else {
            self.set_error("Failed to resolve bundled VDD installer path".to_string());
            return;
        };

        if !script.exists() {
            self.set_error(format!(
                "Bundled VDD installer not found: {}",
                script.display()
            ));
            return;
        }

        if let Err(error) = crate::process::hidden_command("powershell.exe")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-WindowStyle",
                "Hidden",
                "-File",
            ])
            .arg(script)
            .args(["-Action", action])
            .spawn()
        {
            self.set_error(format!("Failed to start VDD action {action}: {error}"));
        }
    }

    fn open_display_settings(&self) {
        if let Err(error) = std::process::Command::new("explorer.exe")
            .arg("ms-settings:display")
            .spawn()
        {
            eprintln!("failed to open display settings: {error}");
        }
    }

    fn open_vdd_page(&self) {
        if let Err(error) = std::process::Command::new("explorer.exe")
            .arg("https://github.com/VirtualDrivers/Virtual-Display-Driver/releases")
            .spawn()
        {
            eprintln!("failed to open VDD page: {error}");
        }
    }

    fn reload_config(&mut self) {
        match AppConfig::load_or_create() {
            Ok((config, path)) => {
                let mode = self.active_mode;
                if let Err(error) = self.stop_current() {
                    self.set_error(format!("Stop failed: {error:#}"));
                    return;
                }
                self.config = config;
                self.config_path = path;
                self.restart_diagnostics_server();
                match mode {
                    ActiveMode::Sender => self.start_sender(),
                    ActiveMode::Receiver => self.start_receiver(),
                    ActiveMode::Idle => self.sync_menu(),
                }
            }
            Err(error) => self.set_error(format!("Config reload failed: {error:#}")),
        }
    }

    fn reload_config_from_disk(&mut self) -> Result<()> {
        let (config, path) = AppConfig::load_or_create()?;
        self.config = config;
        self.config_path = path;
        self.restart_diagnostics_server();
        self.sync_menu();
        Ok(())
    }

    fn save_config(&self) {
        if let Err(error) = self.config.save_to(&self.config_path) {
            eprintln!("failed to save config: {error:#}");
        }
    }

    fn sync_menu(&self) {
        let Some(items) = self.items.as_ref() else {
            return;
        };

        match self.active_mode {
            ActiveMode::Idle => items.status.set_text("Status: stopped"),
            ActiveMode::Sender => items.status.set_text(format!(
                "Status: sending to {}:{}",
                if self.config.send.host.eq_ignore_ascii_case("auto") {
                    "auto".to_string()
                } else {
                    self.config.send.host.clone()
                },
                self.config.send.port
            )),
            ActiveMode::Receiver => items
                .status
                .set_text(format!("Status: receiving on :{}", self.config.recv.port)),
        }

        items
            .start_sender
            .set_enabled(self.active_mode != ActiveMode::Sender);
        items
            .start_receiver
            .set_enabled(self.active_mode != ActiveMode::Receiver);
        items.stop.set_enabled(self.active_mode != ActiveMode::Idle);
        items.autostart.set_text(if self.config.autostart {
            "Disable Autostart"
        } else {
            "Enable Autostart"
        });
    }

    fn poll_update_status(&self) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        while let Ok(message) = self.update_status_rx.try_recv() {
            items.status.set_text(message);
        }
    }

    fn set_error(&self, message: String) {
        eprintln!("{message}");
        crate::logging::append(&message);
        if let Some(items) = self.items.as_ref() {
            items.status.set_text(format!("Error: {message}"));
        }
    }

    fn restart_diagnostics_server(&mut self) {
        if let Some(server) = self.diagnostics_server.take() {
            server.stop();
        }
        match crate::diagnostics::DiagnosticsServer::start(&self.config.security.pin) {
            Ok(server) => self.diagnostics_server = Some(server),
            Err(error) => self.set_error(format!("Diagnostics server failed: {error:#}")),
        }
    }
}

impl TrayItems {
    fn new(autostart_enabled: bool) -> Result<Self> {
        let status = MenuItem::with_id("status", "Status: stopped", false, None);
        let start_sender = MenuItem::with_id(ID_START_SENDER, "Start as Sender", true, None);
        let start_receiver = MenuItem::with_id(ID_START_RECEIVER, "Start as Receiver", true, None);
        let stop = MenuItem::with_id(ID_STOP, "Stop", false, None);
        let autostart = MenuItem::with_id(
            ID_AUTOSTART,
            if autostart_enabled {
                "Disable Autostart"
            } else {
                "Enable Autostart"
            },
            true,
            None,
        );

        Ok(Self {
            status,
            start_sender,
            start_receiver,
            stop,
            autostart,
        })
    }
}

fn app_tooltip() -> String {
    format!("Screen Mirror v{}", env!("CARGO_PKG_VERSION"))
}

fn app_icon() -> Result<Icon> {
    if let Some(icon_path) = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(|parent| parent.join("screen-mirror.ico")))
    {
        if icon_path.exists() {
            if let Ok(icon) = Icon::from_path(icon_path, Some((32, 32))) {
                return Ok(icon);
            }
        }
    }

    let size = 32;
    let mut rgba = Vec::with_capacity(size * size * 4);
    let (color_r, color_g, color_b) = (248, 181, 0);
    for y in 0..size {
        for x in 0..size {
            let border = (3..=28).contains(&x)
                && (6..=22).contains(&y)
                && (x <= 5 || x >= 26 || y <= 8 || y >= 20);
            let divider = (15..=17).contains(&x) && (7..=21).contains(&y);
            let left_arrow = (9..=16).contains(&x) && (13..=17).contains(&y)
                || (12..=16).contains(&x) && (10..=20).contains(&y) && x + y >= 26;
            let right_arrow = (16..=23).contains(&x) && (13..=17).contains(&y)
                || (16..=20).contains(&x) && (10..=20).contains(&y) && x + y <= 36;
            let stand = (10..=21).contains(&x) && (26..=27).contains(&y);
            let (red, green, blue, alpha) =
                if border || divider || left_arrow || right_arrow || stand {
                    (color_r, color_g, color_b, 255)
                } else {
                    (0, 0, 0, 0)
                };
            rgba.extend_from_slice(&[red, green, blue, alpha]);
        }
    }
    Icon::from_rgba(rgba, size as u32, size as u32).context("failed to create tray icon image")
}
