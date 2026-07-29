use anyhow::{Context, Result};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::autostart;
use crate::config::{AppConfig, StartupMode};
use crate::pipeline::{self, PipelineHandle};

const ID_START_SENDER: &str = "start-sender";
const ID_START_RECEIVER: &str = "start-receiver";
const ID_STOP: &str = "stop";
const ID_TOGGLE_AUDIO: &str = "toggle-audio";
const ID_GPU_SEND_PREFIX: &str = "gpu-send:";
const ID_GPU_RECV_PREFIX: &str = "gpu-recv:";
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
const TRAY_MENU_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TRAY_RIGHT_CLICK_FALLBACK: Duration = Duration::from_millis(350);
const TRAY_MENU_REOPEN_GUARD: Duration = Duration::from_millis(750);

#[derive(Debug)]
enum UserEvent {
    Menu(MenuEvent),
    Tray(TrayIconEvent),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ActiveMode {
    Idle,
    Sender,
    Receiver,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum GpuSide {
    Sender,
    Receiver,
}

impl GpuSide {
    fn label(self) -> &'static str {
        match self {
            Self::Sender => "sender",
            Self::Receiver => "receiver",
        }
    }
}

pub fn run() -> Result<()> {
    crate::logging::append("tray run start");
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        if let Err(error) = menu_proxy.send_event(UserEvent::Menu(event)) {
            crate::logging::append(format!("failed to deliver tray menu event: {error}"));
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event| {
        if let Err(error) = proxy.send_event(UserEvent::Tray(event)) {
            crate::logging::append(format!("failed to deliver tray icon event: {error}"));
        }
    }));

    let mut app = TrayApp::new()?;
    crate::logging::append(format!(
        "config loaded: startup_mode={:?} autostart={}",
        app.config.startup_mode, app.config.autostart
    ));
    app.initialize_tray()?;
    crate::logging::append("tray initialized");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(Instant::now() + TRAY_MENU_POLL_INTERVAL);

        match event {
            Event::UserEvent(UserEvent::Menu(event)) => {
                app.handle_menu(event.id().as_ref(), control_flow);
            }
            Event::UserEvent(UserEvent::Tray(event)) => app.handle_tray_event(event),
            Event::LoopDestroyed => {
                let _ = app.stop_current();
            }
            _ => {}
        }

        app.poll_tray_menu_fallback();
        app.poll_update_status();
        app.reap_finished_pipeline();
        app.reap_finished_audio_pipeline();
    });
}

struct TrayApp {
    config: AppConfig,
    config_path: std::path::PathBuf,
    active_mode: ActiveMode,
    pipeline: Option<PipelineHandle>,
    audio_pipeline: Option<PipelineHandle>,
    sender_supervisor: Option<crate::lan::SenderSupervisor>,
    control_server: Option<crate::control::ControlServer>,
    diagnostics_server: Option<crate::diagnostics::DiagnosticsServer>,
    announcer: Option<crate::lan::Announcer>,
    sleep_guard: Option<crate::power::SleepGuard>,
    render_window: Option<crate::receiver_window::RenderWindowGuard>,
    update_status_tx: Sender<String>,
    update_status_rx: Receiver<String>,
    tray: Option<TrayIcon>,
    menu: Option<Menu>,
    #[cfg(windows)]
    menu_owner: Option<crate::tray_menu_owner::TrayMenuOwner>,
    pending_tray_right_click: Option<Instant>,
    last_tray_menu_closed: Option<Instant>,
    items: Option<TrayItems>,
}

struct TrayItems {
    status: MenuItem,
    start_sender: MenuItem,
    start_receiver: MenuItem,
    stop: MenuItem,
    audio: MenuItem,
    autostart: MenuItem,
    /// Empty when this machine has at most one GPU, so no GPU menu is shown at all.
    gpu_send: Vec<GpuMenuChoice>,
    gpu_recv: Vec<GpuMenuChoice>,
}

struct GpuMenuChoice {
    /// Value written to config: "auto" or the adapter name.
    selection: String,
    item: CheckMenuItem,
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
            audio_pipeline: None,
            sender_supervisor: None,
            control_server: None,
            diagnostics_server: None,
            announcer: None,
            sleep_guard: None,
            render_window: None,
            update_status_tx,
            update_status_rx,
            tray: None,
            menu: None,
            #[cfg(windows)]
            menu_owner: None,
            pending_tray_right_click: None,
            last_tray_menu_closed: None,
            items: None,
        })
    }

    fn initialize_tray(&mut self) -> Result<()> {
        crate::logging::append("initialize_tray");
        if self.tray.is_some() {
            return Ok(());
        }

        let items = TrayItems::new(&self.config)?;
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
        menu.append(&items.audio)?;
        if !items.gpu_send.is_empty() {
            menu.append(&PredefinedMenuItem::separator())?;
            menu.append(&gpu_submenu("Sender GPU", &items.gpu_send)?)?;
            menu.append(&gpu_submenu("Receiver GPU", &items.gpu_recv)?)?;
        }
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

        let tray_builder = TrayIconBuilder::new()
            .with_tooltip(app_tooltip())
            .with_icon(app_icon()?)
            .with_menu_on_left_click(false);

        #[cfg(windows)]
        let tray = tray_builder
            .with_menu_on_right_click(false)
            .build()
            .context("failed to create tray icon")?;

        #[cfg(not(windows))]
        let tray = tray_builder
            .with_menu(Box::new(menu.clone()))
            .with_menu_on_right_click(true)
            .build()
            .context("failed to create tray icon")?;

        self.items = Some(items);
        self.menu = Some(menu);
        #[cfg(windows)]
        {
            self.menu_owner = Some(crate::tray_menu_owner::TrayMenuOwner::new()?);
        }
        self.tray = Some(tray);
        self.sync_menu();
        crate::updater::start_background_update_checks(self.update_status_tx.clone());
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
            ID_TOGGLE_AUDIO => self.toggle_audio(),
            id if id.starts_with(ID_GPU_SEND_PREFIX) => self.select_gpu(GpuSide::Sender, id),
            id if id.starts_with(ID_GPU_RECV_PREFIX) => self.select_gpu(GpuSide::Receiver, id),
            ID_AUTOSTART => self.toggle_autostart(),
            ID_CHECK_UPDATE => self.check_for_updates(),
            ID_RUN_DIAGNOSTICS => self.run_diagnostics(),
            ID_RUN_PEER_DIAGNOSTICS => self.run_peer_diagnostics(),
            ID_INSTALL_VDD => self.run_vdd_action(crate::vdd::VddAction::Install),
            ID_LIST_VDD => self.open_vdd_status(),
            ID_ENABLE_VDD => self.run_vdd_action(crate::vdd::VddAction::Enable),
            ID_DISABLE_VDD => self.run_vdd_action(crate::vdd::VddAction::Disable),
            ID_REMOVE_VDD => self.run_vdd_action(crate::vdd::VddAction::Remove),
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

    fn handle_tray_event(&mut self, event: TrayIconEvent) {
        match event {
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Down,
                ..
            } => {
                self.pending_tray_right_click = Some(Instant::now());
                crate::logging::append("tray right button down received");
            }
            TrayIconEvent::Click {
                button: MouseButton::Right,
                button_state: MouseButtonState::Up,
                ..
            } => {
                self.pending_tray_right_click = None;
                self.show_tray_menu("button-up");
            }
            _ => {}
        }
    }

    fn poll_tray_menu_fallback(&mut self) {
        let Some(pressed_at) = self.pending_tray_right_click else {
            return;
        };
        if pressed_at.elapsed() < TRAY_RIGHT_CLICK_FALLBACK {
            return;
        }
        self.pending_tray_right_click = None;
        self.show_tray_menu("button-up fallback");
    }

    fn show_tray_menu(&mut self, trigger: &str) {
        if self
            .last_tray_menu_closed
            .is_some_and(|closed_at| closed_at.elapsed() < TRAY_MENU_REOPEN_GUARD)
        {
            crate::logging::append(format!("tray menu duplicate suppressed: trigger={trigger}"));
            return;
        }

        crate::logging::append(format!("tray right click received: trigger={trigger}"));
        #[cfg(windows)]
        if let (Some(menu), Some(owner)) = (self.menu.as_ref(), self.menu_owner.as_ref()) {
            owner.show(menu);
            self.last_tray_menu_closed = Some(Instant::now());
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
            Err(error) => {
                crate::logging::append(format!("touch control server failed: {error:#}"));
                eprintln!("touch control server failed: {error:#}");
            }
        }
        match crate::lan::Announcer::sender(
            self.config.send.port,
            Some(self.config.send.audio_port),
            &self.config.security.pin,
        ) {
            Ok(announcer) => self.announcer = Some(announcer),
            Err(error) => {
                crate::logging::append(format!("sender discovery announce failed: {error:#}"));
                eprintln!("sender discovery announce failed: {error:#}");
            }
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
        let video_description = match pipeline::build_sender_video_pipeline(&args) {
            Ok(description) => description,
            Err(error) => {
                self.set_error(format!("Sender start failed: {error:#}"));
                return;
            }
        };
        let audio_description = if args.audio_enabled {
            match pipeline::build_sender_audio_pipeline(&args) {
                Ok(description) => Some(description),
                Err(error) => {
                    self.set_error(format!("Sender audio start failed: {error:#}"));
                    return;
                }
            }
        } else {
            None
        };

        eprintln!("sender video pipeline: {video_description}");
        crate::logging::append(format!("sender pipeline started: target={}", args.host));
        self.pipeline = Some(pipeline::spawn_pipeline(video_description));
        if let Some(description) = audio_description {
            crate::logging::append(format!(
                "sender audio pipeline started: target={}",
                args.host
            ));
            self.audio_pipeline = Some(pipeline::spawn_pipeline(description));
        }
        self.active_mode = ActiveMode::Sender;
        self.config.startup_mode = StartupMode::Sender;
        self.save_config();
        self.sync_menu();
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
        let video_description = match pipeline::build_receiver_video_pipeline(&args) {
            Ok(description) => description,
            Err(error) => {
                self.set_error(format!("Receiver start failed: {error:#}"));
                return;
            }
        };
        let audio_description = if args.audio_enabled {
            match pipeline::build_receiver_audio_pipeline(&args) {
                Ok(description) => Some(description),
                Err(error) => {
                    self.set_error(format!("Receiver audio start failed: {error:#}"));
                    return;
                }
            }
        } else {
            None
        };

        crate::logging::append(format!("receiver video pipeline: {video_description}"));
        eprintln!("receiver video pipeline: {video_description}");
        self.pipeline = Some(pipeline::spawn_pipeline(video_description));
        if let Some(description) = audio_description {
            crate::logging::append(format!("receiver audio pipeline: {description}"));
            self.audio_pipeline = Some(pipeline::spawn_pipeline(description));
        }
        self.sleep_guard = Some(crate::power::SleepGuard::receiver());
        self.render_window = Some(crate::receiver_window::RenderWindowGuard::start());
        match crate::lan::Announcer::receiver(
            self.config.recv.port,
            Some(self.config.recv.audio_port),
            &self.config.security.pin,
        ) {
            Ok(announcer) => self.announcer = Some(announcer),
            Err(error) => {
                crate::logging::append(format!("receiver discovery announce failed: {error:#}"));
                eprintln!("receiver discovery announce failed: {error:#}");
            }
        }
        self.active_mode = ActiveMode::Receiver;
        self.config.startup_mode = StartupMode::Receiver;
        self.save_config();
        self.sync_menu();
    }

    fn stop_current(&mut self) -> Result<()> {
        let mut first_error = None;
        if let Some(handle) = self.audio_pipeline.take() {
            if let Err(error) = handle.stop() {
                first_error = Some(error);
            }
        }
        if let Some(handle) = self.pipeline.take() {
            if let Err(error) = handle.stop() {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
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
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
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

        if result.is_ok() && self.active_mode == ActiveMode::Receiver {
            let args = self.config.recv_args();
            match pipeline::build_receiver_video_pipeline(&args) {
                Ok(description) => {
                    crate::logging::append(
                        "receiver stream disconnected; fullscreen closed and listener restarted",
                    );
                    crate::logging::append(format!("receiver pipeline: {description}"));
                    self.pipeline = Some(pipeline::spawn_pipeline(description));
                    self.sync_menu();
                    return;
                }
                Err(error) => {
                    self.set_error(format!("Receiver restart failed: {error:#}"));
                }
            }
        }

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
        if let Some(handle) = self.audio_pipeline.take() {
            if let Err(error) = handle.stop() {
                crate::logging::append(format!("audio pipeline cleanup failed: {error:#}"));
            }
        }
        self.render_window = None;
        self.sleep_guard = None;
        self.active_mode = ActiveMode::Idle;
        self.config.startup_mode = StartupMode::Idle;
        self.save_config();
        self.sync_menu();
    }

    fn reap_finished_audio_pipeline(&mut self) {
        let Some(handle) = self.audio_pipeline.as_ref() else {
            return;
        };
        if !handle.is_finished() {
            return;
        }

        let result = self
            .audio_pipeline
            .take()
            .map(PipelineHandle::finish)
            .unwrap_or(Ok(()));
        match self.active_mode {
            ActiveMode::Sender => self.config.send.audio_enabled = false,
            ActiveMode::Receiver => self.config.recv.audio_enabled = false,
            ActiveMode::Idle => {}
        }
        self.save_config();
        self.sync_menu();

        match result {
            Ok(()) => crate::logging::append(
                "audio pipeline stopped independently; video session remains active",
            ),
            Err(error) => self.set_error(format!(
                "Audio pipeline stopped; video session remains active: {error:#}"
            )),
        }
    }

    /// Applies a GPU pick from the tray menu, then restarts the running session on that side so
    /// the new GPU is actually used.
    fn select_gpu(&mut self, side: GpuSide, id: &str) {
        let selection = {
            let Some(items) = self.items.as_ref() else {
                return;
            };
            let choices = match side {
                GpuSide::Sender => &items.gpu_send,
                GpuSide::Receiver => &items.gpu_recv,
            };
            match choices
                .iter()
                .find(|choice| choice.item.id().as_ref() == id)
            {
                Some(choice) => choice.selection.clone(),
                None => return,
            }
        };

        let current = match side {
            GpuSide::Sender => &self.config.send.gpu,
            GpuSide::Receiver => &self.config.recv.gpu,
        };
        if gpu_choice_is_active(current, &selection) {
            self.sync_menu();
            return;
        }

        match side {
            GpuSide::Sender => self.config.send.gpu = selection.clone(),
            GpuSide::Receiver => self.config.recv.gpu = selection.clone(),
        }
        self.save_config();
        crate::logging::append(format!("{} GPU set to {selection}", side.label()));
        self.sync_menu();

        match (side, self.active_mode) {
            (GpuSide::Sender, ActiveMode::Sender) => self.start_sender(),
            (GpuSide::Receiver, ActiveMode::Receiver) => self.start_receiver(),
            _ => {}
        }
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

    fn toggle_audio(&mut self) {
        let previous_send = self.config.send.audio_enabled;
        let previous_recv = self.config.recv.audio_enabled;
        let enabled = !(self.config.send.audio_enabled && self.config.recv.audio_enabled);
        self.config.send.audio_enabled = enabled;
        self.config.recv.audio_enabled = enabled;

        if let Err(error) = self.config.save_to(&self.config_path) {
            self.config.send.audio_enabled = previous_send;
            self.config.recv.audio_enabled = previous_recv;
            self.set_error(format!("Audio setting update failed: {error:#}"));
            return;
        }

        crate::logging::append(format!(
            "system audio transfer {}",
            if enabled { "enabled" } else { "disabled" }
        ));

        if let Err(error) = self.apply_audio_toggle(enabled) {
            if enabled {
                self.config.send.audio_enabled = previous_send;
                self.config.recv.audio_enabled = previous_recv;
                self.save_config();
            }
            self.set_error(format!("Audio setting update failed: {error:#}"));
            return;
        }
        crate::logging::append(
            "system audio pipeline changed without restarting the video session",
        );
        self.sync_menu();
    }

    fn apply_audio_toggle(&mut self, enabled: bool) -> Result<()> {
        match self.active_mode {
            ActiveMode::Sender => {
                if let Some(supervisor) = self.sender_supervisor.as_ref() {
                    return supervisor.set_audio_enabled(enabled);
                }
                if enabled {
                    if self.audio_pipeline.is_none() {
                        let args = self.config.send_args();
                        let description = pipeline::build_sender_audio_pipeline(&args)?;
                        self.audio_pipeline = Some(pipeline::spawn_pipeline(description));
                    }
                } else if let Some(handle) = self.audio_pipeline.take() {
                    handle.stop()?;
                }
            }
            ActiveMode::Receiver => {
                if enabled {
                    if self.audio_pipeline.is_none() {
                        let args = self.config.recv_args();
                        let description = pipeline::build_receiver_audio_pipeline(&args)?;
                        self.audio_pipeline = Some(pipeline::spawn_pipeline(description));
                    }
                } else if let Some(handle) = self.audio_pipeline.take() {
                    handle.stop()?;
                }
            }
            ActiveMode::Idle => {}
        }
        Ok(())
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

    fn run_vdd_action(&self, action: crate::vdd::VddAction) {
        if let Err(error) = crate::vdd::request(action, 1) {
            self.set_error(format!("Failed to run VDD action {action:?}: {error:#}"));
        }
    }

    /// Writes the same display report the old PowerShell status action produced, then shows it.
    fn open_vdd_status(&self) {
        let mut report = vec![
            "Screen Mirror Virtual Display Status".to_string(),
            String::new(),
        ];
        let monitors = crate::monitors::enumerate_monitors();
        if monitors.is_empty() {
            report.push("No Windows display devices found.".to_string());
        }
        for monitor in &monitors {
            report.push(monitor.summary());
        }
        report.push(String::new());
        let targets = crate::monitors::bundled_virtual_capture_targets();
        report.push(format!(
            "Bundled virtual displays ready to capture: {}",
            targets.len()
        ));
        for target in &targets {
            report.push(format!("  {}", target.adapter_name));
        }

        let path = std::env::temp_dir().join("ScreenMirror-vdd-status.txt");
        if let Err(error) = std::fs::write(&path, report.join("\r\n")) {
            self.set_error(format!("Failed to write the display report: {error}"));
            return;
        }
        if let Err(error) = std::process::Command::new("notepad.exe").arg(&path).spawn() {
            self.set_error(format!("Failed to open the display report: {error}"));
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
                "Status: sending to {}:{} ({})",
                if self.config.send.host.eq_ignore_ascii_case("auto") {
                    "auto".to_string()
                } else {
                    self.config.send.host.clone()
                },
                self.config.send.port,
                audio_status(self.config.send.audio_enabled)
            )),
            ActiveMode::Receiver => items.status.set_text(format!(
                "Status: receiving on :{} ({})",
                self.config.recv.port,
                audio_status(self.config.recv.audio_enabled)
            )),
        }

        items
            .start_sender
            .set_enabled(self.active_mode != ActiveMode::Sender);
        items
            .start_receiver
            .set_enabled(self.active_mode != ActiveMode::Receiver);
        items.stop.set_enabled(self.active_mode != ActiveMode::Idle);
        items.audio.set_text(
            if self.config.send.audio_enabled && self.config.recv.audio_enabled {
                "Disable System Audio Transfer"
            } else {
                "Enable System Audio Transfer"
            },
        );
        for choice in &items.gpu_send {
            choice.item.set_checked(gpu_choice_is_active(
                &self.config.send.gpu,
                &choice.selection,
            ));
        }
        for choice in &items.gpu_recv {
            choice.item.set_checked(gpu_choice_is_active(
                &self.config.recv.gpu,
                &choice.selection,
            ));
        }
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
    fn new(config: &AppConfig) -> Result<Self> {
        let autostart_enabled = config.autostart;
        let status = MenuItem::with_id("status", "Status: stopped", false, None);
        let start_sender = MenuItem::with_id(ID_START_SENDER, "Start as Sender", true, None);
        let start_receiver = MenuItem::with_id(ID_START_RECEIVER, "Start as Receiver", true, None);
        let stop = MenuItem::with_id(ID_STOP, "Stop", false, None);
        let audio = MenuItem::with_id(ID_TOGGLE_AUDIO, "Enable System Audio Transfer", true, None);
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

        // A single-GPU machine has nothing to choose, so it gets no GPU menu.
        let adapters = crate::gpu::adapters();
        let (gpu_send, gpu_recv) = if adapters.len() > 1 {
            (
                gpu_choices(ID_GPU_SEND_PREFIX, &adapters, &config.send.gpu),
                gpu_choices(ID_GPU_RECV_PREFIX, &adapters, &config.recv.gpu),
            )
        } else {
            (Vec::new(), Vec::new())
        };

        Ok(Self {
            status,
            start_sender,
            start_receiver,
            stop,
            audio,
            autostart,
            gpu_send,
            gpu_recv,
        })
    }
}

fn gpu_submenu(title: &str, choices: &[GpuMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new(title, true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn gpu_choices(
    id_prefix: &str,
    adapters: &[crate::gpu::GpuAdapter],
    configured: &str,
) -> Vec<GpuMenuChoice> {
    let mut choices = vec![GpuMenuChoice {
        item: CheckMenuItem::with_id(
            format!("{id_prefix}{}", crate::gpu::AUTO),
            "Automatic",
            true,
            gpu_choice_is_active(configured, crate::gpu::AUTO),
            None,
        ),
        selection: crate::gpu::AUTO.to_string(),
    }];

    for adapter in adapters {
        let (selection, label) = gpu_choice_selection_and_label(adapters, adapter);
        choices.push(GpuMenuChoice {
            item: CheckMenuItem::with_id(
                format!("{id_prefix}{}", adapter.index),
                &label,
                true,
                gpu_choice_is_active(configured, &selection),
                None,
            ),
            selection,
        });
    }
    choices
}

/// Two adapters can report the same description, and then the name identifies neither of them;
/// those fall back to the DXGI index, which at least stays unambiguous.
fn gpu_choice_selection_and_label(
    adapters: &[crate::gpu::GpuAdapter],
    adapter: &crate::gpu::GpuAdapter,
) -> (String, String) {
    let ambiguous_name = adapters
        .iter()
        .filter(|other| other.description.eq_ignore_ascii_case(&adapter.description))
        .count()
        > 1;
    if ambiguous_name {
        (
            adapter.index.to_string(),
            format!("{} (GPU {})", adapter.description, adapter.index),
        )
    } else {
        (adapter.selection(), adapter.description.clone())
    }
}

fn gpu_choice_is_active(configured: &str, selection: &str) -> bool {
    if crate::gpu::is_auto(selection) {
        return crate::gpu::is_auto(configured);
    }
    !crate::gpu::is_auto(configured) && configured.eq_ignore_ascii_case(selection)
}

fn audio_status(enabled: bool) -> &'static str {
    if enabled {
        "audio on"
    } else {
        "audio off"
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuAdapter;

    fn adapter(index: u32, description: &str) -> GpuAdapter {
        GpuAdapter {
            index,
            luid: index as i64 + 1,
            vendor_id: 0x10DE,
            description: description.to_string(),
        }
    }

    #[test]
    fn distinct_gpu_names_are_stored_by_name() {
        let adapters = vec![
            adapter(0, "NVIDIA GeForce RTX 4060 Ti"),
            adapter(1, "AMD Radeon Graphics"),
        ];
        let (selection, label) = gpu_choice_selection_and_label(&adapters, &adapters[1]);
        assert_eq!(selection, "AMD Radeon Graphics");
        assert_eq!(label, "AMD Radeon Graphics");
    }

    #[test]
    fn repeated_gpu_names_fall_back_to_the_adapter_index() {
        let adapters = vec![
            adapter(0, "Intel(R) HD Graphics"),
            adapter(1, "Intel(R) HD Graphics"),
        ];
        let (selection, label) = gpu_choice_selection_and_label(&adapters, &adapters[1]);
        assert_eq!(selection, "1");
        assert_eq!(label, "Intel(R) HD Graphics (GPU 1)");
    }

    #[test]
    fn only_the_configured_gpu_choice_is_checked() {
        assert!(gpu_choice_is_active("auto", crate::gpu::AUTO));
        assert!(gpu_choice_is_active("", crate::gpu::AUTO));
        assert!(!gpu_choice_is_active("1", crate::gpu::AUTO));
        assert!(gpu_choice_is_active(
            "amd radeon graphics",
            "AMD Radeon Graphics"
        ));
        assert!(!gpu_choice_is_active("auto", "AMD Radeon Graphics"));
        assert!(!gpu_choice_is_active("0", "1"));
    }
}
