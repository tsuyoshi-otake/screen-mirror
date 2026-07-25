use anyhow::{Context, Result};
use tao::event::{Event, StartCause};
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
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = proxy.send_event(UserEvent::Menu(event));
    }));

    let mut app = TrayApp::new()?;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::NewEvents(StartCause::Init) => {
                if let Err(error) = app.initialize_tray() {
                    eprintln!("{error:?}");
                }
            }
            Event::UserEvent(UserEvent::Menu(event)) => {
                app.handle_menu(event.id().as_ref(), control_flow);
            }
            Event::LoopDestroyed => {
                let _ = app.stop_current();
            }
            _ => {}
        }
    });
}

struct TrayApp {
    config: AppConfig,
    config_path: std::path::PathBuf,
    active_mode: ActiveMode,
    pipeline: Option<PipelineHandle>,
    sender_supervisor: Option<crate::lan::SenderSupervisor>,
    control_server: Option<crate::control::ControlServer>,
    announcer: Option<crate::lan::Announcer>,
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

        Ok(Self {
            config,
            config_path,
            active_mode: ActiveMode::Idle,
            pipeline: None,
            sender_supervisor: None,
            control_server: None,
            announcer: None,
            tray: None,
            items: None,
        })
    }

    fn initialize_tray(&mut self) -> Result<()> {
        if self.tray.is_some() {
            return Ok(());
        }

        let items = TrayItems::new(self.config.autostart)?;
        let menu = Menu::new();
        let sep1 = PredefinedMenuItem::separator();
        let sep2 = PredefinedMenuItem::separator();
        let sep3 = PredefinedMenuItem::separator();
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
        menu.append(&open_config)?;
        menu.append(&reload_config)?;
        menu.append(&sep3)?;
        menu.append(&quit)?;

        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("screen-mirror")
            .with_icon(app_icon()?)
            .with_menu_on_left_click(true)
            .with_menu_on_right_click(true)
            .build()
            .context("failed to create tray icon")?;

        self.items = Some(items);
        self.tray = Some(tray);
        self.sync_menu();
        crate::updater::start_background_update_checks();

        if let Err(error) = autostart::set_enabled(self.config.autostart) {
            self.set_error(format!("Autostart update failed: {error:#}"));
        }

        match self.config.startup_mode {
            StartupMode::Idle => {}
            StartupMode::Sender => self.start_sender(),
            StartupMode::Receiver => self.start_receiver(),
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
            ID_OPEN_CONFIG => self.open_config(),
            ID_RELOAD_CONFIG => self.reload_config(),
            ID_QUIT => {
                let _ = self.stop_current();
                *control_flow = ControlFlow::Exit;
            }
            _ => {}
        }
    }

    fn start_sender(&mut self) {
        if let Err(error) = self.stop_current() {
            self.set_error(format!("Stop failed: {error:#}"));
            return;
        }

        let args: crate::pipeline::SendArgs = self.config.send.clone().into();
        match crate::control::ControlServer::start() {
            Ok(server) => self.control_server = Some(server),
            Err(error) => eprintln!("touch control server failed: {error:#}"),
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
        if let Err(error) = self.stop_current() {
            self.set_error(format!("Stop failed: {error:#}"));
            return;
        }

        let args = self.config.recv.clone().into();
        match pipeline::build_receiver_pipeline(&args) {
            Ok(description) => {
                eprintln!("receiver pipeline: {description}");
                self.pipeline = Some(pipeline::spawn_pipeline(description));
                match crate::lan::Announcer::receiver(self.config.recv.port) {
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
        self.active_mode = ActiveMode::Idle;
        self.sync_menu();
        Ok(())
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

    fn open_config(&self) {
        if let Err(error) = std::process::Command::new("notepad.exe")
            .arg(&self.config_path)
            .spawn()
        {
            eprintln!("failed to open config: {error}");
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
                match mode {
                    ActiveMode::Sender => self.start_sender(),
                    ActiveMode::Receiver => self.start_receiver(),
                    ActiveMode::Idle => self.sync_menu(),
                }
            }
            Err(error) => self.set_error(format!("Config reload failed: {error:#}")),
        }
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

    fn set_error(&self, message: String) {
        eprintln!("{message}");
        if let Some(items) = self.items.as_ref() {
            items.status.set_text(format!("Error: {message}"));
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

fn app_icon() -> Result<Icon> {
    let size = 32;
    let mut rgba = Vec::with_capacity(size * size * 4);

    for y in 0..size {
        for x in 0..size {
            let border = x < 3 || y < 3 || x >= size - 3 || y >= size - 3;
            let beam = x > 8 && x < 24 && y > 10 && y < 22;
            let (r, g, b, a) = if border {
                (40, 180, 255, 255)
            } else if beam {
                (120, 220, 255, 255)
            } else {
                (10, 24, 40, 255)
            };
            rgba.extend_from_slice(&[r, g, b, a]);
        }
    }

    Icon::from_rgba(rgba, size as u32, size as u32).context("failed to create tray icon image")
}
