use anyhow::{Context, Result};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use crate::autostart;
use crate::config::{AppConfig, ConfigEncoder, ConfigNvidiaTuning, ConfigSampling, StartupMode};
use crate::pipeline::{self, PipelineHandle};

const ID_START_SENDER: &str = "start-sender";
const ID_START_RECEIVER: &str = "start-receiver";
const ID_STOP: &str = "stop";
const ID_TOGGLE_AUDIO: &str = "toggle-audio";
const ID_GPU_SEND_PREFIX: &str = "gpu-send:";
const ID_GPU_RECV_PREFIX: &str = "gpu-recv:";
const ID_FRAME_RATE_PREFIX: &str = "fps:";
const ID_SAMPLING_PREFIX: &str = "sampling:";
const ID_ENCODER_PREFIX: &str = "encoder:";
const ID_QUALITY_PREFIX: &str = "quality:";
const ID_BITRATE_PREFIX: &str = "bitrate:";
const ID_FEC_PREFIX: &str = "fec:";
const ID_RESOLUTION_PREFIX: &str = "resolution:";
const ID_NVIDIA_TUNING_PREFIX: &str = "nvidia-tuning:";
const ID_AUTOSTART: &str = "autostart";
const ID_CHECK_UPDATE: &str = "check-update";
const ID_RUN_DIAGNOSTICS: &str = "run-diagnostics";
const ID_RUN_PEER_DIAGNOSTICS: &str = "run-peer-diagnostics";
const ID_INSTALL_VDD: &str = "install-vdd";
const ID_LIST_VDD: &str = "list-vdd";
const ID_ATTACH_VDD: &str = "attach-vdd";
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
                let _ = app.end_session("event loop destroyed");
            }
            _ => {}
        }

        app.poll_tray_menu_fallback();
        app.poll_status_messages();
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
    feedback_sender: Option<crate::lan::FeedbackSender>,
    receiver_stats: Option<Arc<Mutex<pipeline::ReceiverStreamStats>>>,
    diagnostics_server: Option<crate::diagnostics::DiagnosticsServer>,
    announcer: Option<crate::lan::Announcer>,
    sleep_guard: Option<crate::power::SleepGuard>,
    render_window: Option<crate::receiver_window::RenderWindowGuard>,
    status_tx: Sender<String>,
    status_rx: Receiver<String>,
    tray: Option<TrayIcon>,
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
    frame_rates: Vec<FrameRateMenuChoice>,
    sampling: Vec<SamplingMenuChoice>,
    encoders: Vec<EncoderMenuChoice>,
    qualities: Vec<QualityMenuChoice>,
    bitrates: Vec<BitrateMenuChoice>,
    fec: Vec<FecMenuChoice>,
    resolutions: Vec<ResolutionMenuChoice>,
    nvidia_tuning: Vec<NvidiaTuningMenuChoice>,
}

struct GpuMenuChoice {
    /// Value written to config: "auto" or the adapter name.
    selection: String,
    item: CheckMenuItem,
}

struct FrameRateMenuChoice {
    fps: u32,
    item: CheckMenuItem,
}

/// Capture rates offered in the tray, in menu order.
///
/// A sender streams at whatever rate it captures, and every frame it does not capture is one the
/// receiver cannot invent. The rate lived only in config.toml, which meant the one setting that
/// decides whether motion looks smooth was the one setting nobody could find; 30 stays the default
/// because it is what a laptop and a Wi-Fi link comfortably sustain, and everything above it is now
/// one click away for the desks where it does not have to be a compromise. 75 and 90 are here
/// because monitors run at them: capturing at a rate the panel does not refresh at beats every
/// frame against the display clock and judders however high the number is.
const CAPTURE_FRAME_RATES: [u32; 5] = [15, 30, 60, 75, 90];

struct SamplingMenuChoice {
    sampling: ConfigSampling,
    item: CheckMenuItem,
}

struct EncoderMenuChoice {
    encoder: ConfigEncoder,
    item: CheckMenuItem,
}

struct QualityMenuChoice {
    preset: QualityPreset,
    item: CheckMenuItem,
}

struct BitrateMenuChoice {
    bitrate: u32,
    item: CheckMenuItem,
}

struct FecMenuChoice {
    percentage: u32,
    item: CheckMenuItem,
}

struct ResolutionMenuChoice {
    width: Option<u32>,
    height: Option<u32>,
    item: CheckMenuItem,
}

struct NvidiaTuningMenuChoice {
    tuning: ConfigNvidiaTuning,
    item: CheckMenuItem,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum QualityPreset {
    Economy,
    Balanced,
    High,
    Maximum,
    Custom,
}

impl QualityPreset {
    const ALL: [Self; 5] = [
        Self::Economy,
        Self::Balanced,
        Self::High,
        Self::Maximum,
        Self::Custom,
    ];

    const fn key(self) -> &'static str {
        match self {
            Self::Economy => "economy",
            Self::Balanced => "balanced",
            Self::High => "high",
            Self::Maximum => "maximum",
            Self::Custom => "custom",
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Economy => "Economy (3 Mbps / low latency)",
            Self::Balanced => "Balanced (8 Mbps)",
            Self::High => "High (16 Mbps / NVENC AQ)",
            Self::Maximum => "Maximum (24 Mbps / NVENC AQ)",
            Self::Custom => "Custom (manual bitrate/tuning)",
        }
    }

    const fn settings(self) -> Option<(u32, ConfigNvidiaTuning)> {
        match self {
            Self::Economy => Some((3_000, ConfigNvidiaTuning::LowLatency)),
            Self::Balanced => Some((8_000, ConfigNvidiaTuning::Auto)),
            Self::High => Some((16_000, ConfigNvidiaTuning::Rtx)),
            Self::Maximum => Some((24_000, ConfigNvidiaTuning::Rtx)),
            Self::Custom => None,
        }
    }

    fn from_settings(bitrate: u32, tuning: ConfigNvidiaTuning) -> Self {
        Self::ALL
            .into_iter()
            .find(|preset| preset.settings() == Some((bitrate, tuning)))
            .unwrap_or(Self::Custom)
    }
}

const VIDEO_BITRATES: [u32; 6] = [3_000, 5_000, 8_000, 12_000, 16_000, 24_000];
const OUTPUT_RESOLUTIONS: [(Option<u32>, Option<u32>, &str); 5] = [
    (None, None, "Native monitor resolution"),
    (Some(1280), Some(720), "1280 × 720"),
    (Some(1920), Some(1080), "1920 × 1080"),
    (Some(2560), Some(1440), "2560 × 1440"),
    (Some(3840), Some(2160), "3840 × 2160"),
];

/// Scaling filters offered in the tray, in menu order, with the label each is shown under.
///
/// The words are the effect, not the filter: nobody picks a mirrored screen by whether the sampler
/// is bilinear. `Sharp` is here for the desks where the stream and the panel are the same size and
/// any filtering at all is pure softening.
const SAMPLING_CHOICES: [(ConfigSampling, &str); 3] = [
    (ConfigSampling::Auto, "Auto (sharp text, smooth scaling)"),
    (ConfigSampling::Linear, "Smooth"),
    (ConfigSampling::Point, "Sharp"),
];

impl TrayApp {
    fn new() -> Result<Self> {
        let (mut config, config_path) = AppConfig::load_or_create()?;
        if let Ok(enabled) = autostart::is_enabled() {
            config.autostart = enabled;
        }
        let (status_tx, status_rx) = mpsc::channel();

        Ok(Self {
            config,
            config_path,
            active_mode: ActiveMode::Idle,
            pipeline: None,
            audio_pipeline: None,
            sender_supervisor: None,
            control_server: None,
            feedback_sender: None,
            receiver_stats: None,
            diagnostics_server: None,
            announcer: None,
            sleep_guard: None,
            render_window: None,
            status_tx,
            status_rx,
            tray: None,
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
        let attach_vdd = MenuItem::with_id(
            ID_ATTACH_VDD,
            "Attach Virtual Displays to Desktop",
            true,
            None,
        );
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
        menu.append(&PredefinedMenuItem::separator())?;
        menu.append(&frame_rate_submenu(&items.frame_rates)?)?;
        menu.append(&encoder_submenu(&items.encoders)?)?;
        menu.append(&quality_submenu(&items.qualities)?)?;
        menu.append(&bitrate_submenu(&items.bitrates)?)?;
        menu.append(&fec_submenu(&items.fec)?)?;
        menu.append(&resolution_submenu(&items.resolutions)?)?;
        menu.append(&nvidia_tuning_submenu(&items.nvidia_tuning)?)?;
        menu.append(&sampling_submenu(&items.sampling)?)?;
        if !items.gpu_send.is_empty() {
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
        menu.append(&attach_vdd)?;
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
            .with_menu(Box::new(menu))
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
            .with_menu_on_right_click(true)
            .build()
            .context("failed to create tray icon")?;

        self.items = Some(items);
        self.tray = Some(tray);
        self.sync_menu();
        crate::updater::start_background_update_checks(self.status_tx.clone());
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
                if let Err(error) = self.end_session("stopped from the tray") {
                    self.set_error(format!("Stop failed: {error:#}"));
                }
                self.config.startup_mode = StartupMode::Idle;
                self.save_config();
            }
            ID_TOGGLE_AUDIO => self.toggle_audio(),
            id if id.starts_with(ID_GPU_SEND_PREFIX) => self.select_gpu(GpuSide::Sender, id),
            id if id.starts_with(ID_GPU_RECV_PREFIX) => self.select_gpu(GpuSide::Receiver, id),
            id if id.starts_with(ID_FRAME_RATE_PREFIX) => self.select_frame_rate(id),
            id if id.starts_with(ID_ENCODER_PREFIX) => self.select_encoder(id),
            id if id.starts_with(ID_QUALITY_PREFIX) => self.select_quality(id),
            id if id.starts_with(ID_BITRATE_PREFIX) => self.select_bitrate(id),
            id if id.starts_with(ID_FEC_PREFIX) => self.select_fec(id),
            id if id.starts_with(ID_RESOLUTION_PREFIX) => self.select_resolution(id),
            id if id.starts_with(ID_NVIDIA_TUNING_PREFIX) => self.select_nvidia_tuning(id),
            id if id.starts_with(ID_SAMPLING_PREFIX) => self.select_sampling(id),
            ID_AUTOSTART => self.toggle_autostart(),
            ID_CHECK_UPDATE => self.check_for_updates(),
            ID_RUN_DIAGNOSTICS => self.run_diagnostics(),
            ID_RUN_PEER_DIAGNOSTICS => self.run_peer_diagnostics(),
            ID_INSTALL_VDD => self.run_vdd_action(crate::vdd::VddAction::Install),
            ID_LIST_VDD => self.open_vdd_status(),
            ID_ATTACH_VDD => self.attach_virtual_displays(),
            ID_ENABLE_VDD => self.run_vdd_action(crate::vdd::VddAction::Enable),
            ID_DISABLE_VDD => self.run_vdd_action(crate::vdd::VddAction::Disable),
            ID_REMOVE_VDD => self.run_vdd_action(crate::vdd::VddAction::Remove),
            ID_OPEN_DISPLAY_SETTINGS => self.open_display_settings(),
            ID_OPEN_VDD => self.open_vdd_page(),
            ID_OPEN_CONFIG => self.open_config(),
            ID_RELOAD_CONFIG => self.reload_config(),
            ID_QUIT => {
                let _ = self.end_session("app quit");
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
        if let Some(tray) = self.tray.as_ref() {
            tray.show_menu();
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
        let feedback = match crate::control::ControlServer::start(&self.config.security.pin) {
            Ok(server) => {
                let feedback = server.feedback();
                self.control_server = Some(server);
                feedback
            }
            Err(error) => {
                crate::logging::append(format!("touch control server failed: {error:#}"));
                eprintln!("touch control server failed: {error:#}");
                crate::control::FeedbackStore::default()
            }
        };
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
            self.sender_supervisor = Some(crate::lan::SenderSupervisor::start(
                args,
                self.status_tx.clone(),
                feedback,
            ));
            self.active_mode = ActiveMode::Sender;
            self.config.startup_mode = StartupMode::Sender;
            self.save_config();
            self.sync_menu();
            return;
        }

        let (args, capture_target) = match crate::lan::resolve_sender_args(args) {
            Ok(resolved) => resolved,
            Err(error) => {
                self.set_error(format!("Receiver discovery failed: {error:#}"));
                return;
            }
        };
        let video_description =
            match pipeline::build_sender_video_pipeline_for(&args, capture_target.as_ref()) {
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
        self.pipeline = Some(pipeline::spawn_sender_pipeline_with_feedback(
            video_description,
            args.bitrate,
            Some(feedback),
        ));
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

        if let Err(error) = self.end_session("switching to receiver mode") {
            self.set_error(format!("Stop failed: {error:#}"));
            return;
        }

        let args = self.config.recv_args();
        let video_plan = match pipeline::build_receiver_video_plan(&args) {
            Ok(plan) => plan,
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

        crate::logging::append(format!("receiver video pipeline: {}", video_plan.primary()));
        eprintln!("receiver video pipeline: {}", video_plan.primary());
        // Announced below so senders can scale to it; read before the plan is handed to the thread.
        let decode_limits = video_plan.decode_limits();
        let receiver_stats = Arc::new(Mutex::new(pipeline::ReceiverStreamStats::default()));
        self.receiver_stats = Some(Arc::clone(&receiver_stats));
        self.pipeline = Some(pipeline::spawn_receiver_pipeline_with_stats(
            video_plan,
            Some(Arc::clone(&receiver_stats)),
        ));
        if let Some(description) = audio_description {
            crate::logging::append(format!("receiver audio pipeline: {description}"));
            self.audio_pipeline = Some(pipeline::spawn_pipeline(description));
        }
        self.sleep_guard = Some(crate::power::SleepGuard::receiver());
        self.render_window = Some(crate::receiver_window::RenderWindowGuard::start(Some(
            Arc::clone(&receiver_stats),
        )));
        match crate::lan::FeedbackSender::start(
            self.config.security.pin.clone(),
            Arc::clone(&receiver_stats),
        ) {
            Ok(sender) => self.feedback_sender = Some(sender),
            Err(error) => {
                crate::logging::append(format!("receiver feedback sender failed: {error:#}"));
                eprintln!("receiver feedback sender failed: {error:#}");
            }
        }
        match crate::lan::Announcer::receiver(
            self.config.recv.port,
            Some(self.config.recv.audio_port),
            &self.config.security.pin,
            decode_limits,
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

    /// Ends the running session and gives back everything it borrowed from the desktop.
    ///
    /// This is the stop for the paths that really finish a session. `start_sender` deliberately
    /// keeps using `stop_current`: restarting the sender would otherwise remove its virtual
    /// displays and immediately reinstall them, behind two UAC prompts instead of none.
    fn end_session(&mut self, reason: &str) -> Result<()> {
        let ending_mode = self.active_mode;
        let result = self.stop_current();
        // Cleanup runs even when a pipeline failed to stop cleanly; a stranded desktop is the more
        // visible failure of the two.
        self.release_sender_virtual_displays(ending_mode, reason);
        result
    }

    /// A sender session owns the virtual displays it brought up. Once it ends there is no receiver
    /// showing them, so leaving them attached strands a desktop nobody can see.
    fn release_sender_virtual_displays(&self, ending_mode: ActiveMode, reason: &str) {
        if !releases_sender_virtual_displays(ending_mode, self.config.send.enable_virtual_display) {
            return;
        }
        crate::logging::append(format!("releasing sender virtual displays: {reason}"));
        crate::monitors::remove_bundled_virtual_display();
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
        if let Some(sender) = self.feedback_sender.take() {
            sender.stop();
        }
        if let Some(announcer) = self.announcer.take() {
            announcer.stop();
        }
        self.render_window = None;
        self.receiver_stats = None;
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
            match pipeline::build_receiver_video_plan(&args) {
                Ok(plan) => {
                    crate::logging::append(
                        "receiver stream disconnected; fullscreen closed and listener restarted",
                    );
                    crate::logging::append(format!("receiver pipeline: {}", plan.primary()));
                    self.pipeline = Some(pipeline::spawn_receiver_pipeline_with_stats(
                        plan,
                        self.receiver_stats.clone(),
                    ));
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
        if let Some(sender) = self.feedback_sender.take() {
            sender.stop();
        }
        if let Some(announcer) = self.announcer.take() {
            announcer.stop();
        }
        if let Some(handle) = self.audio_pipeline.take() {
            if let Err(error) = handle.stop() {
                crate::logging::append(format!("audio pipeline cleanup failed: {error:#}"));
            }
        }
        // Released only once nothing is capturing any more, so the driver is not torn out from
        // under a running pipeline.
        self.release_sender_virtual_displays(self.active_mode, "sender pipeline stopped");
        self.render_window = None;
        self.receiver_stats = None;
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

    /// Applies a capture rate from the tray menu, and restarts a running sender so the next frame
    /// is captured at it. A receiver keeps running untouched: the rate belongs to whoever captures.
    fn select_frame_rate(&mut self, id: &str) {
        let Some(fps) = frame_rate_for_menu_id(id) else {
            return;
        };
        if self.config.send.fps == fps {
            self.sync_menu();
            return;
        }

        self.config.send.fps = fps;
        self.save_config();
        crate::logging::append(format!("sender capture frame rate set to {fps} fps"));
        self.sync_menu();

        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies the sender encoder selected from the tray and restarts the sender when needed.
    fn select_encoder(&mut self, id: &str) {
        let Some(encoder) = self
            .items
            .as_ref()
            .and_then(|items| {
                items
                    .encoders
                    .iter()
                    .find(|choice| choice.item.id().as_ref() == id)
            })
            .map(|choice| choice.encoder)
        else {
            return;
        };
        if self.config.send.encoder == encoder {
            self.sync_menu();
            return;
        }

        self.config.send.encoder = encoder;
        self.save_config();
        crate::logging::append(format!("sender encoder set to {}", encoder.key()));
        self.sync_menu();
        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies a quality preset by changing the existing bitrate and NVIDIA tuning settings as a
    /// bundle. The encoder family itself is left untouched so a preset remains safe on AMD, Intel,
    /// and CPU-only machines; NVIDIA tuning is simply ignored by other encoder families.
    fn select_quality(&mut self, id: &str) {
        let Some(preset) = quality_for_menu_id(id) else {
            return;
        };
        let Some((bitrate, tuning)) = preset.settings() else {
            self.sync_menu();
            return;
        };
        if self.config.send.bitrate == bitrate && self.config.send.nvidia_tuning == tuning {
            self.sync_menu();
            return;
        }

        self.config.send.bitrate = bitrate;
        self.config.send.nvidia_tuning = tuning;
        self.save_config();
        crate::logging::append(format!(
            "sender video quality preset set to {}",
            preset.key()
        ));
        self.sync_menu();
        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies a sender bitrate in kbit/s. The pacer and encoder are rebuilt together so the
    /// network pacing follows the selected quality instead of bursting at the old rate.
    fn select_bitrate(&mut self, id: &str) {
        let Some(bitrate) = bitrate_for_menu_id(id) else {
            return;
        };
        if self.config.send.bitrate == bitrate {
            self.sync_menu();
            return;
        }

        self.config.send.bitrate = bitrate;
        self.save_config();
        crate::logging::append(format!("sender video bitrate set to {bitrate} kbit/s"));
        self.sync_menu();
        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies RFC 5109 video packet-loss protection and restarts a running sender so the new
    /// redundancy stream is negotiated with the selected receivers.
    fn select_fec(&mut self, id: &str) {
        let Some(percentage) = fec_for_menu_id(id) else {
            return;
        };
        if self.config.send.fec_percentage == percentage {
            self.sync_menu();
            return;
        }

        self.config.send.fec_percentage = percentage;
        self.save_config();
        crate::logging::append(format!(
            "sender video packet-loss protection set to {percentage}% FEC overhead"
        ));
        self.sync_menu();

        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies an output size. Native resolution keeps the existing monitor dimensions; fixed
    /// sizes reduce the encoded pixels before the encoder and are useful on slower links.
    fn select_resolution(&mut self, id: &str) {
        let Some((width, height)) = resolution_for_menu_id(id) else {
            return;
        };
        if self.config.send.width == width && self.config.send.height == height {
            self.sync_menu();
            return;
        }

        self.config.send.width = width;
        self.config.send.height = height;
        self.save_config();
        crate::logging::append(format!(
            "sender output resolution set to {}",
            resolution_label(width, height)
        ));
        self.sync_menu();
        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Selects the NVIDIA encoder tuning profile. The setting is harmless on other encoder
    /// families and becomes active automatically if the encoder is later switched to NVENC.
    fn select_nvidia_tuning(&mut self, id: &str) {
        let Some(tuning) = nvidia_tuning_for_menu_id(id) else {
            return;
        };
        if self.config.send.nvidia_tuning == tuning {
            self.sync_menu();
            return;
        }

        self.config.send.nvidia_tuning = tuning;
        self.save_config();
        crate::logging::append(format!("NVIDIA tuning set to {}", tuning.key()));
        self.sync_menu();
        if self.active_mode == ActiveMode::Sender {
            self.start_sender();
        }
    }

    /// Applies a scaling filter from the tray menu. The filter is a property of the video sink, so
    /// it only takes effect once the sink is built: a running receiver restarts, a sender does not
    /// care. Restarting drops a fraction of a second of video, which is why an unchanged choice
    /// only re-ticks the menu.
    fn select_sampling(&mut self, id: &str) {
        let Some(sampling) = sampling_for_menu_id(id) else {
            return;
        };
        if self.config.recv.sampling == sampling {
            self.sync_menu();
            return;
        }

        self.config.recv.sampling = sampling;
        self.save_config();
        crate::logging::append(format!("receiver scaling filter set to {}", sampling.key()));
        self.sync_menu();

        if self.active_mode == ActiveMode::Receiver {
            self.start_receiver();
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
        crate::updater::start_manual_update_check(self.status_tx.clone());
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

    /// Manual counterpart to the attach the sender performs on start, for repairing a detached
    /// virtual display without waiting for the next session.
    fn attach_virtual_displays(&self) {
        if crate::monitors::attach_detached_bundled_virtual_displays() {
            let ready = crate::monitors::bundled_virtual_capture_targets().len();
            if let Some(items) = self.items.as_ref() {
                items
                    .status
                    .set_text(format!("Status: {ready} virtual display(s) capture-ready"));
            }
        } else {
            self.set_error("No detached virtual display could be attached".to_string());
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

        let detached: Vec<&crate::monitors::DisplayMonitor> = monitors
            .iter()
            .filter(|monitor| monitor.bundled_virtual_display && !monitor.attached)
            .collect();
        report.push(format!(
            "Bundled virtual displays detached from the desktop: {}",
            detached.len()
        ));
        for monitor in &detached {
            report.push(format!("  {}", monitor.adapter_name));
        }
        if !detached.is_empty() {
            report.push(String::new());
            report.push(
                "A detached virtual display cannot be captured. Use \"Attach Virtual Displays to Desktop\", or extend the desktop with Win+P."
                    .to_string(),
            );
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
        // A sender's own busy state comes from its supervisor, which knows whether any pipeline is
        // actually running; the tray only knows which mode was selected.
        crate::updater::set_session_active(self.active_mode == ActiveMode::Receiver);
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
        for choice in &items.frame_rates {
            choice.item.set_checked(choice.fps == self.config.send.fps);
        }
        for choice in &items.encoders {
            choice
                .item
                .set_checked(choice.encoder == self.config.send.encoder);
        }
        let quality =
            QualityPreset::from_settings(self.config.send.bitrate, self.config.send.nvidia_tuning);
        for choice in &items.qualities {
            choice.item.set_checked(choice.preset == quality);
        }
        for choice in &items.bitrates {
            choice
                .item
                .set_checked(choice.bitrate == self.config.send.bitrate);
        }
        for choice in &items.fec {
            choice
                .item
                .set_checked(choice.percentage == self.config.send.fec_percentage);
        }
        for choice in &items.resolutions {
            choice.item.set_checked(
                choice.width == self.config.send.width && choice.height == self.config.send.height,
            );
        }
        for choice in &items.nvidia_tuning {
            choice
                .item
                .set_checked(choice.tuning == self.config.send.nvidia_tuning);
        }
        for choice in &items.sampling {
            choice
                .item
                .set_checked(choice.sampling == self.config.recv.sampling);
        }
        items.autostart.set_text(if self.config.autostart {
            "Disable Autostart"
        } else {
            "Enable Autostart"
        });
    }

    fn poll_status_messages(&self) {
        let Some(items) = self.items.as_ref() else {
            return;
        };
        while let Ok(message) = self.status_rx.try_recv() {
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
            frame_rates: frame_rate_choices(config.send.fps),
            sampling: sampling_choices(config.recv.sampling),
            encoders: encoder_choices(config.send.encoder),
            qualities: quality_choices(config.send.bitrate, config.send.nvidia_tuning),
            bitrates: bitrate_choices(config.send.bitrate),
            fec: fec_choices(config.send.fec_percentage),
            resolutions: resolution_choices(config.send.width, config.send.height),
            nvidia_tuning: nvidia_tuning_choices(config.send.nvidia_tuning),
        })
    }
}

fn frame_rate_submenu(choices: &[FrameRateMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Capture Frame Rate", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn frame_rate_choices(configured: u32) -> Vec<FrameRateMenuChoice> {
    frame_rate_menu_rates(configured)
        .into_iter()
        .map(|fps| FrameRateMenuChoice {
            fps,
            item: CheckMenuItem::with_id(
                format!("{ID_FRAME_RATE_PREFIX}{fps}"),
                format!("{fps} fps"),
                true,
                fps == configured,
                None,
            ),
        })
        .collect()
}

/// The offered rates, plus whatever rate config.toml already asks for.
///
/// A hand-edited `fps = 90` is a deliberate choice, and a menu that cannot show it would leave the
/// user looking at a list where nothing is ticked and any click silently discards their setting.
fn frame_rate_menu_rates(configured: u32) -> Vec<u32> {
    let mut rates = CAPTURE_FRAME_RATES.to_vec();
    if configured > 0 && !rates.contains(&configured) {
        rates.push(configured);
        rates.sort_unstable();
    }
    rates
}

fn frame_rate_for_menu_id(id: &str) -> Option<u32> {
    id.strip_prefix(ID_FRAME_RATE_PREFIX)?.parse().ok()
}

fn encoder_submenu(choices: &[EncoderMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Video Encoder", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn encoder_choices(configured: ConfigEncoder) -> Vec<EncoderMenuChoice> {
    ConfigEncoder::ALL
        .into_iter()
        .map(|encoder| EncoderMenuChoice {
            encoder,
            item: CheckMenuItem::with_id(
                format!("{ID_ENCODER_PREFIX}{}", encoder.key()),
                encoder.label(),
                true,
                encoder == configured,
                None,
            ),
        })
        .collect()
}

fn quality_submenu(choices: &[QualityMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Video Quality Preset", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn quality_choices(
    configured_bitrate: u32,
    configured_tuning: ConfigNvidiaTuning,
) -> Vec<QualityMenuChoice> {
    let configured = QualityPreset::from_settings(configured_bitrate, configured_tuning);
    QualityPreset::ALL
        .into_iter()
        .map(|preset| QualityMenuChoice {
            preset,
            item: CheckMenuItem::with_id(
                format!("{ID_QUALITY_PREFIX}{}", preset.key()),
                preset.label(),
                preset != QualityPreset::Custom,
                preset == configured,
                None,
            ),
        })
        .collect()
}

fn quality_for_menu_id(id: &str) -> Option<QualityPreset> {
    let key = id.strip_prefix(ID_QUALITY_PREFIX)?;
    QualityPreset::ALL
        .into_iter()
        .find(|preset| preset.key() == key)
}

fn bitrate_submenu(choices: &[BitrateMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Video Bitrate", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn bitrate_choices(configured: u32) -> Vec<BitrateMenuChoice> {
    let mut rates = VIDEO_BITRATES.to_vec();
    if configured > 0 && !rates.contains(&configured) {
        rates.push(configured);
        rates.sort_unstable();
    }
    rates
        .into_iter()
        .map(|bitrate| BitrateMenuChoice {
            bitrate,
            item: CheckMenuItem::with_id(
                format!("{ID_BITRATE_PREFIX}{bitrate}"),
                format!("{:.1} Mbps", bitrate as f32 / 1_000.0),
                true,
                bitrate == configured,
                None,
            ),
        })
        .collect()
}

fn bitrate_for_menu_id(id: &str) -> Option<u32> {
    id.strip_prefix(ID_BITRATE_PREFIX)?.parse().ok()
}

fn fec_submenu(choices: &[FecMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Packet Loss Protection", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

const FEC_PERCENTAGES: [u32; 4] = [0, 10, 25, 50];

fn fec_choices(configured: u32) -> Vec<FecMenuChoice> {
    let mut percentages = FEC_PERCENTAGES.to_vec();
    if configured <= 100 && !percentages.contains(&configured) {
        percentages.push(configured);
        percentages.sort_unstable();
    }
    percentages
        .into_iter()
        .map(|percentage| FecMenuChoice {
            percentage,
            item: CheckMenuItem::with_id(
                format!("{ID_FEC_PREFIX}{percentage}"),
                if percentage == 0 {
                    "Off".to_string()
                } else {
                    format!("{percentage}% overhead")
                },
                true,
                percentage == configured,
                None,
            ),
        })
        .collect()
}

fn fec_for_menu_id(id: &str) -> Option<u32> {
    let percentage = id.strip_prefix(ID_FEC_PREFIX)?.parse().ok()?;
    (percentage <= 100).then_some(percentage)
}

fn resolution_submenu(choices: &[ResolutionMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Output Resolution", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn resolution_choices(
    configured_width: Option<u32>,
    configured_height: Option<u32>,
) -> Vec<ResolutionMenuChoice> {
    let mut choices = OUTPUT_RESOLUTIONS
        .into_iter()
        .map(|(width, height, label)| ResolutionMenuChoice {
            width,
            height,
            item: CheckMenuItem::with_id(
                format!("{ID_RESOLUTION_PREFIX}{}", resolution_key(width, height)),
                label,
                true,
                width == configured_width && height == configured_height,
                None,
            ),
        })
        .collect::<Vec<_>>();

    if !choices
        .iter()
        .any(|choice| choice.width == configured_width && choice.height == configured_height)
    {
        let label = resolution_label(configured_width, configured_height);
        choices.push(ResolutionMenuChoice {
            width: configured_width,
            height: configured_height,
            item: CheckMenuItem::with_id(
                format!(
                    "{ID_RESOLUTION_PREFIX}{}",
                    resolution_key(configured_width, configured_height)
                ),
                label,
                true,
                true,
                None,
            ),
        });
    }
    choices
}

fn resolution_key(width: Option<u32>, height: Option<u32>) -> String {
    match (width, height) {
        (Some(width), Some(height)) => format!("{width}x{height}"),
        _ => "native".to_string(),
    }
}

fn resolution_label(width: Option<u32>, height: Option<u32>) -> String {
    match (width, height) {
        (Some(width), Some(height)) => format!("{width} × {height}"),
        _ => "Native monitor resolution".to_string(),
    }
}

fn resolution_for_menu_id(id: &str) -> Option<(Option<u32>, Option<u32>)> {
    let key = id.strip_prefix(ID_RESOLUTION_PREFIX)?;
    if key == "native" {
        return Some((None, None));
    }
    let (width, height) = key.split_once('x')?;
    Some((Some(width.parse().ok()?), Some(height.parse().ok()?)))
}

fn nvidia_tuning_submenu(choices: &[NvidiaTuningMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("NVIDIA Tuning", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn nvidia_tuning_choices(configured: ConfigNvidiaTuning) -> Vec<NvidiaTuningMenuChoice> {
    ConfigNvidiaTuning::ALL
        .into_iter()
        .map(|tuning| NvidiaTuningMenuChoice {
            tuning,
            item: CheckMenuItem::with_id(
                format!("{ID_NVIDIA_TUNING_PREFIX}{}", tuning.key()),
                tuning.label(),
                true,
                tuning == configured,
                None,
            ),
        })
        .collect()
}

fn nvidia_tuning_for_menu_id(id: &str) -> Option<ConfigNvidiaTuning> {
    let key = id.strip_prefix(ID_NVIDIA_TUNING_PREFIX)?;
    ConfigNvidiaTuning::ALL
        .into_iter()
        .find(|tuning| tuning.key() == key)
}

fn sampling_submenu(choices: &[SamplingMenuChoice]) -> Result<Submenu> {
    let submenu = Submenu::new("Scaling Filter", true);
    for choice in choices {
        submenu.append(&choice.item)?;
    }
    Ok(submenu)
}

fn sampling_choices(configured: ConfigSampling) -> Vec<SamplingMenuChoice> {
    SAMPLING_CHOICES
        .into_iter()
        .map(|(sampling, label)| SamplingMenuChoice {
            sampling,
            item: CheckMenuItem::with_id(
                format!("{ID_SAMPLING_PREFIX}{}", sampling.key()),
                label,
                true,
                sampling == configured,
                None,
            ),
        })
        .collect()
}

fn sampling_for_menu_id(id: &str) -> Option<ConfigSampling> {
    let key = id.strip_prefix(ID_SAMPLING_PREFIX)?;
    SAMPLING_CHOICES
        .into_iter()
        .find_map(|(sampling, _)| (sampling.key() == key).then_some(sampling))
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

/// Whether the session that is ending has virtual displays of its own to hand back.
///
/// Only a sender brings them up, and only when it is configured to. A receiver-mode or idle stop
/// must leave the driver alone: the device may have been installed by hand from the tray menu, and
/// removing it needs elevation, so a stop that cannot own the displays must not prompt for one.
fn releases_sender_virtual_displays(ending_mode: ActiveMode, enable_virtual_display: bool) -> bool {
    ending_mode == ActiveMode::Sender && enable_virtual_display
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::GpuAdapter;

    #[test]
    fn only_a_sender_session_hands_back_its_virtual_displays() {
        assert!(releases_sender_virtual_displays(ActiveMode::Sender, true));
        assert!(!releases_sender_virtual_displays(
            ActiveMode::Receiver,
            true
        ));
        assert!(!releases_sender_virtual_displays(ActiveMode::Idle, true));
    }

    #[test]
    fn a_sender_that_never_created_virtual_displays_leaves_the_driver_alone() {
        assert!(!releases_sender_virtual_displays(ActiveMode::Sender, false));
    }

    #[test]
    fn the_frame_rate_menu_offers_the_standard_rates() {
        assert_eq!(frame_rate_menu_rates(30), vec![15, 30, 60, 75, 90]);
    }

    #[test]
    fn a_hand_edited_frame_rate_stays_visible_and_selectable() {
        assert_eq!(frame_rate_menu_rates(120), vec![15, 30, 60, 75, 90, 120]);
        assert_eq!(frame_rate_menu_rates(24), vec![15, 24, 30, 60, 75, 90]);
    }

    #[test]
    fn a_frame_rate_menu_click_resolves_to_the_rate_it_shows() {
        let choices = frame_rate_choices(60);

        assert_eq!(
            choices.iter().filter(|choice| choice.fps == 60).count(),
            1,
            "the configured rate must appear exactly once or the tick is ambiguous"
        );
        for choice in &choices {
            assert_eq!(
                frame_rate_for_menu_id(choice.item.id().as_ref()),
                Some(choice.fps)
            );
        }
    }

    #[test]
    fn menu_ids_from_other_submenus_are_not_read_as_frame_rates() {
        assert_eq!(frame_rate_for_menu_id("gpu-send:0"), None);
        assert_eq!(frame_rate_for_menu_id("fps:auto"), None);
    }

    #[test]
    fn encoder_menu_ids_round_trip_to_the_configured_encoders() {
        let choices = encoder_choices(ConfigEncoder::Auto);

        assert_eq!(choices.len(), ConfigEncoder::ALL.len());
        for choice in &choices {
            assert!(choice.item.id().as_ref().starts_with(ID_ENCODER_PREFIX));
            assert_eq!(
                choice.encoder.key(),
                choice.item.id().as_ref().split(':').nth(1).unwrap()
            );
        }
    }

    #[test]
    fn bitrate_menu_keeps_a_hand_edited_value_visible() {
        let choices = bitrate_choices(10_000);

        assert_eq!(
            choices
                .iter()
                .filter(|choice| choice.bitrate == 10_000)
                .count(),
            1
        );
        for choice in &choices {
            assert_eq!(
                bitrate_for_menu_id(choice.item.id().as_ref()),
                Some(choice.bitrate)
            );
        }
        assert_eq!(bitrate_for_menu_id("encoder:auto"), None);
    }

    #[test]
    fn fec_menu_defaults_and_hand_edited_values_round_trip() {
        let choices = fec_choices(25);

        assert_eq!(
            choices
                .iter()
                .map(|choice| choice.percentage)
                .collect::<Vec<_>>(),
            vec![0, 10, 25, 50]
        );
        for choice in &choices {
            assert_eq!(
                fec_for_menu_id(choice.item.id().as_ref()),
                Some(choice.percentage)
            );
        }

        let custom = fec_choices(40);
        assert_eq!(
            custom
                .iter()
                .map(|choice| choice.percentage)
                .collect::<Vec<_>>(),
            vec![0, 10, 25, 40, 50]
        );
        assert_eq!(fec_for_menu_id("bitrate:8000"), None);
        assert_eq!(fec_for_menu_id("fec:101"), None);
    }

    #[test]
    fn quality_presets_round_trip_and_mark_manual_values_custom() {
        let choices = quality_choices(8_000, ConfigNvidiaTuning::Auto);

        assert_eq!(choices.len(), QualityPreset::ALL.len());
        for choice in &choices {
            assert_eq!(
                quality_for_menu_id(choice.item.id().as_ref()),
                Some(choice.preset)
            );
        }
        assert_eq!(
            QualityPreset::from_settings(8_000, ConfigNvidiaTuning::Auto),
            QualityPreset::Balanced
        );
        assert_eq!(
            QualityPreset::from_settings(10_000, ConfigNvidiaTuning::Auto),
            QualityPreset::Custom
        );
        assert_eq!(quality_for_menu_id("bitrate:8000"), None);
    }

    #[test]
    fn resolution_menu_round_trips_native_and_fixed_sizes() {
        let choices = resolution_choices(Some(1600), Some(900));

        assert_eq!(
            choices
                .iter()
                .filter(|choice| choice.width == Some(1600))
                .count(),
            1
        );
        assert_eq!(
            resolution_for_menu_id("resolution:native"),
            Some((None, None))
        );
        assert_eq!(
            resolution_for_menu_id("resolution:1920x1080"),
            Some((Some(1920), Some(1080)))
        );
        assert_eq!(resolution_for_menu_id("resolution:bad"), None);
    }

    #[test]
    fn nvidia_tuning_menu_ids_round_trip_to_profiles() {
        let choices = nvidia_tuning_choices(ConfigNvidiaTuning::Auto);

        assert_eq!(choices.len(), ConfigNvidiaTuning::ALL.len());
        for choice in &choices {
            assert_eq!(
                nvidia_tuning_for_menu_id(choice.item.id().as_ref()),
                Some(choice.tuning)
            );
        }
        assert_eq!(nvidia_tuning_for_menu_id("nvidia-tuning:unknown"), None);
    }

    #[test]
    fn a_scaling_filter_click_resolves_to_the_filter_it_shows() {
        let choices = sampling_choices(ConfigSampling::Auto);

        assert_eq!(choices.len(), SAMPLING_CHOICES.len());
        for choice in &choices {
            assert_eq!(
                sampling_for_menu_id(choice.item.id().as_ref()),
                Some(choice.sampling)
            );
        }
    }

    /// The menu ids are the config.toml spellings, so a click can never write a value that a later
    /// hand edit of the same file would not round-trip.
    #[test]
    fn the_scaling_filter_menu_ids_are_the_config_values() {
        assert_eq!(
            sampling_choices(ConfigSampling::Point)
                .iter()
                .map(|choice| choice.item.id().as_ref().to_string())
                .collect::<Vec<_>>(),
            vec!["sampling:auto", "sampling:linear", "sampling:point"]
        );
    }

    #[test]
    fn menu_ids_from_other_submenus_are_not_read_as_scaling_filters() {
        assert_eq!(sampling_for_menu_id("fps:60"), None);
        assert_eq!(sampling_for_menu_id("sampling:bilinear"), None);
    }

    fn adapter(index: u32, description: &str) -> GpuAdapter {
        GpuAdapter {
            index,
            luid: index as i64 + 1,
            vendor_id: 0x10DE,
            device_id: 0,
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
