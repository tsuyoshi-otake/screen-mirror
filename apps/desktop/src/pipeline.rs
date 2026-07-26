use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Args, ValueEnum};
use gst::prelude::*;
use gstreamer as gst;
use sm_core::discovery::DEFAULT_PIN;
use std::fmt;
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

#[derive(Args, Clone, Debug)]
pub struct SendArgs {
    /// Receiver IPs/hosts separated by commas, or "auto" for LAN discovery.
    #[arg(long, default_value = "auto")]
    pub host: String,

    /// Receiver UDP port. All selected receivers use the same port.
    #[arg(long, default_value_t = 5004)]
    pub port: u16,

    /// Enable low-latency Opus/RTP system-audio loopback transfer.
    #[arg(long, default_value_t = false, action = ArgAction::Set)]
    pub audio_enabled: bool,

    /// Receiver UDP audio port.
    #[arg(long, default_value_t = 5005)]
    pub audio_port: u16,

    /// Opus audio bitrate in bit/sec.
    #[arg(long, default_value_t = 96_000)]
    pub audio_bitrate: u32,

    /// Opus frame size in ms: 2.5, 5, 10, 20, 40, or 60.
    #[arg(long, default_value = "5")]
    pub audio_frame_ms: String,

    /// Maximum receivers to auto-connect to when host is "auto".
    #[arg(long, default_value_t = 3)]
    pub max_receivers: u32,

    /// Four-digit PIN used for LAN discovery pairing.
    #[arg(long, default_value = DEFAULT_PIN)]
    pub pin: String,

    /// Prefer a detected VDD/SuperDisplay-style virtual monitor when monitor-index is -1.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub prefer_virtual_display: bool,

    /// Ask Windows to switch to extended desktop before sending.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub enable_virtual_display: bool,

    /// Match the preferred virtual display mode to the first receiver display.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub sync_virtual_display_resolution: bool,

    /// Capture monitor index. -1 means primary monitor.
    #[arg(long, default_value_t = -1)]
    pub monitor_index: i32,

    /// Capture frame rate.
    #[arg(long, default_value_t = 60)]
    pub fps: u32,

    /// Target bitrate in kbit/sec.
    #[arg(long, default_value_t = 12_000)]
    pub bitrate: u32,

    /// RTP MTU. 1200 is safe for Wi-Fi and VPN-ish paths.
    #[arg(long, default_value_t = 1200)]
    pub mtu: u32,

    /// UDP kernel send buffer size in bytes.
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    pub udp_buffer_size: u32,

    /// DSCP value for UDP video packets. -1 keeps OS default; 46 requests Expedited Forwarding.
    #[arg(long, default_value_t = -1)]
    pub qos_dscp: i32,

    /// Permit auto encoder selection to fall back to CPU x264.
    #[arg(long, default_value_t = false, action = ArgAction::Set)]
    pub allow_software_encoder: bool,

    /// NVIDIA NVENC tuning profile.
    #[arg(long, value_enum, default_value_t = NvidiaTuning::Auto)]
    pub nvidia_tuning: NvidiaTuning,

    /// Encoder to use. auto prefers GPU encoders.
    #[arg(long, value_enum, default_value_t = Encoder::Auto)]
    pub encoder: Encoder,

    /// Capture API. dxgi is lowest overhead; wgc can behave better with modern Windows/window capture.
    #[arg(long, value_enum, default_value_t = CaptureApi::Dxgi)]
    pub capture_api: CaptureApi,

    /// Disable cursor capture.
    #[arg(long)]
    pub no_cursor: bool,

    /// Optional output width. Omit to keep native monitor size.
    #[arg(long)]
    pub width: Option<u32>,

    /// Optional output height. Omit to keep native monitor size.
    #[arg(long)]
    pub height: Option<u32>,
}

#[derive(Args, Clone, Debug)]
pub struct RecvArgs {
    /// Local UDP port to listen on.
    #[arg(long, default_value_t = 5004)]
    pub port: u16,

    /// Enable low-latency Opus/RTP audio playback.
    #[arg(long, default_value_t = false, action = ArgAction::Set)]
    pub audio_enabled: bool,

    /// Local UDP audio port to listen on.
    #[arg(long, default_value_t = 5005)]
    pub audio_port: u16,

    /// RTP audio jitter buffer latency in milliseconds.
    #[arg(long, default_value_t = 15)]
    pub audio_jitter_ms: u32,

    /// Four-digit PIN advertised for LAN discovery pairing.
    #[arg(long, default_value = DEFAULT_PIN)]
    pub pin: String,

    /// RTP jitter buffer latency in milliseconds.
    #[arg(long, default_value_t = 15)]
    pub jitter_ms: u32,

    /// UDP kernel receive buffer size in bytes.
    #[arg(long, default_value_t = 4 * 1024 * 1024)]
    pub udp_buffer_size: u32,

    /// Maximum expected RTP packet size.
    #[arg(long, default_value_t = 1200)]
    pub mtu: u32,

    /// Packets needed before jitterbuffer starts output.
    #[arg(long, default_value_t = 2)]
    pub jitter_faststart_packets: u32,

    /// Maximum missing-packet tolerance in milliseconds.
    #[arg(long, default_value_t = 200)]
    pub jitter_max_dropout_ms: u32,

    /// Maximum misordered-packet tolerance in milliseconds.
    #[arg(long, default_value_t = 50)]
    pub jitter_max_misorder_ms: u32,

    /// Render receiver video fullscreen.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub fullscreen: bool,

    /// Decoder to use. auto prefers D3D11 GPU decode.
    #[arg(long, value_enum, default_value_t = Decoder::Auto)]
    pub decoder: Decoder,

    /// Video sink to use. auto prefers D3D11 GPU rendering.
    #[arg(long, value_enum, default_value_t = Sink::Auto)]
    pub sink: Sink,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Encoder {
    Auto,
    Nvidia,
    MediaFoundation,
    QuickSync,
    X264,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum NvidiaTuning {
    #[default]
    Auto,
    Gtx,
    Rtx,
    LowLatency,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Decoder {
    Auto,
    D3d11,
    Avdec,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Sink {
    Auto,
    D3d11,
    AutoVideo,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum CaptureApi {
    Dxgi,
    Wgc,
}

impl fmt::Display for CaptureApi {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Dxgi => formatter.write_str("dxgi"),
            Self::Wgc => formatter.write_str("wgc"),
        }
    }
}

pub struct PipelineHandle {
    stop: Sender<()>,
    thread: Option<JoinHandle<Result<()>>>,
}

impl PipelineHandle {
    pub fn stop(mut self) -> Result<()> {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("pipeline thread panicked"))??;
        }
        Ok(())
    }
}

impl Drop for PipelineHandle {
    fn drop(&mut self) {
        let _ = self.stop.send(());
    }
}

pub fn spawn_pipeline(description: String) -> PipelineHandle {
    let (stop, stop_rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        let result = run_pipeline_until_stop(&description, stop_rx);
        if let Err(error) = &result {
            crate::logging::append(format!("pipeline failed: {error:#}"));
        }
        result
    });
    PipelineHandle {
        stop,
        thread: Some(thread),
    }
}

pub fn build_sender_pipeline(args: &SendArgs) -> Result<String> {
    ensure_positive("fps", args.fps)?;
    ensure_positive("bitrate", args.bitrate)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("max-receivers", args.max_receivers)?;
    ensure_positive("audio-bitrate", args.audio_bitrate)?;
    validate_qos_dscp(args.qos_dscp)?;
    validate_audio_frame_ms(&args.audio_frame_ms)?;

    if args.width.is_some() != args.height.is_some() {
        return Err(anyhow!("--width and --height must be specified together"));
    }
    if args.host.trim().is_empty() || args.host.trim().eq_ignore_ascii_case("auto") {
        return Err(anyhow!(
            "sender host was not resolved; run discovery first or pass explicit --host"
        ));
    }

    let encoder = select_encoder(args.encoder, args.allow_software_encoder)?;
    let clients = multi_udp_clients(&args.host, args.port)?;
    let monitor_index = crate::monitors::resolve_capture_monitor_index(
        args.monitor_index,
        args.prefer_virtual_display,
    );
    let source = format!(
        "d3d11screencapturesrc capture-api={} monitor-index={} show-cursor={}",
        args.capture_api,
        monitor_index,
        if args.no_cursor { "false" } else { "true" }
    );
    let caps = video_caps(
        args.fps,
        args.width,
        args.height,
        encoder.uses_d3d11_input(),
    );
    let encoder_chain = encoder.chain(args.bitrate, args.fps, args.nvidia_tuning);

    let video = format!(
        "{source} ! queue max-size-buffers=1 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {caps} \
         ! {encoder_chain} \
         ! h264parse config-interval=-1 \
         ! rtph264pay pt=96 mtu={} config-interval=-1 aggregate-mode=zero-latency \
         ! multiudpsink clients={} sync=false async=false buffer-size={} qos-dscp={} send-duplicates=false ttl=1",
        args.mtu,
        gst_string_literal(&clients),
        args.udp_buffer_size,
        args.qos_dscp
    );

    if !args.audio_enabled {
        return Ok(video);
    }

    Ok(format!("{video} {}", build_sender_audio_chain(args)?))
}

pub fn build_receiver_pipeline(args: &RecvArgs) -> Result<String> {
    ensure_positive("jitter-ms", args.jitter_ms)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("jitter-faststart-packets", args.jitter_faststart_packets)?;
    ensure_positive("jitter-max-dropout-ms", args.jitter_max_dropout_ms)?;
    ensure_positive("jitter-max-misorder-ms", args.jitter_max_misorder_ms)?;
    ensure_positive("audio-jitter-ms", args.audio_jitter_ms)?;

    let decoder = select_decoder(args.decoder)?;
    let sink = select_sink(args.sink, args.fullscreen)?;

    let video = format!(
        "udpsrc port={} buffer-size={} mtu={} retrieve-sender-address=false caps=\"application/x-rtp,media=(string)video,clock-rate=(int)90000,encoding-name=(string)H264,payload=(int)96,packetization-mode=(string)1\" \
         ! queue max-size-buffers=32 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! rtpjitterbuffer latency={} drop-on-latency=true do-lost=false faststart-min-packets={} max-dropout-time={} max-misorder-time={} \
         ! rtph264depay \
         ! h264parse disable-passthrough=true \
         ! queue max-size-buffers=2 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {decoder} \
         ! queue max-size-buffers=1 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {sink}",
        args.port,
        args.udp_buffer_size,
        args.mtu,
        args.jitter_ms,
        args.jitter_faststart_packets,
        args.jitter_max_dropout_ms,
        args.jitter_max_misorder_ms
    );

    if !args.audio_enabled {
        return Ok(video);
    }

    Ok(format!("{video} {}", build_receiver_audio_chain(args)?))
}

fn build_sender_audio_chain(args: &SendArgs) -> Result<String> {
    require_element("wasapi2src", ())?;
    require_element("audioconvert", ())?;
    require_element("audioresample", ())?;
    require_element("opusenc", ())?;
    require_element("rtpopuspay", ())?;
    require_element("multiudpsink", ())?;
    let clients = multi_udp_clients_force_port(&args.host, args.audio_port)?;
    Ok(format!(
        "wasapi2src loopback=true low-latency=true buffer-time=10000 latency-time=2500 provide-clock=false \
         ! queue max-size-buffers=4 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! audioconvert ! audioresample ! audio/x-raw,format=S16LE,rate=48000,channels=2 \
         ! opusenc bitrate={} bitrate-type=cbr audio-type=restricted-lowdelay frame-size={} inband-fec=false dtx=false \
         ! rtpopuspay pt=97 mtu={} perfect-rtptime=true \
         ! multiudpsink clients={} sync=false async=false buffer-size={} qos-dscp={} send-duplicates=false ttl=1",
        args.audio_bitrate,
        args.audio_frame_ms,
        args.mtu,
        gst_string_literal(&clients),
        args.udp_buffer_size,
        args.qos_dscp
    ))
}

fn build_receiver_audio_chain(args: &RecvArgs) -> Result<String> {
    require_element("rtpopusdepay", ())?;
    require_element("opusdec", ())?;
    require_element("audioconvert", ())?;
    require_element("audioresample", ())?;
    let sink = if has_element("wasapi2sink") {
        "wasapi2sink low-latency=true buffer-time=10000 latency-time=2500 sync=false async=false"
    } else {
        "autoaudiosink sync=false"
    };
    Ok(format!(
        "udpsrc port={} buffer-size={} mtu={} retrieve-sender-address=false caps=\"application/x-rtp,media=(string)audio,clock-rate=(int)48000,encoding-name=(string)OPUS,payload=(int)97\" \
         ! queue max-size-buffers=32 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! rtpjitterbuffer latency={} drop-on-latency=true do-lost=false faststart-min-packets=2 \
         ! rtpopusdepay ! opusdec plc=true \
         ! queue max-size-buffers=4 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! audioconvert ! audioresample ! {sink}",
        args.audio_port, args.udp_buffer_size, args.mtu, args.audio_jitter_ms
    ))
}

pub fn run_pipeline(description: &str) -> Result<()> {
    let (_stop, stop_rx) = mpsc::channel();
    run_pipeline_until_stop(description, stop_rx)
}

fn run_pipeline_until_stop(description: &str, stop_rx: mpsc::Receiver<()>) -> Result<()> {
    let element = gst::parse::launch(description).context("failed to parse GStreamer pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("pipeline description did not create a GstPipeline"))?;
    let bus = pipeline.bus().context("pipeline has no bus")?;

    pipeline
        .set_state(gst::State::Playing)
        .context("failed to set pipeline to Playing")?;

    let result = loop {
        if stop_rx.try_recv().is_ok() {
            break Ok(());
        }

        if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => break Ok(()),
                MessageView::Error(error) => {
                    let src = error
                        .src()
                        .map(|src| src.path_string())
                        .map(|src| src.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let debug = error.debug().unwrap_or_else(|| "no debug info".into());
                    break Err(anyhow!(
                        "GStreamer error from {src}: {} ({debug})",
                        error.error()
                    ));
                }
                MessageView::Warning(warning) => {
                    let src = warning
                        .src()
                        .map(|src| src.path_string())
                        .map(|src| src.to_string())
                        .unwrap_or_else(|| "unknown".to_string());
                    let debug = warning.debug().unwrap_or_else(|| "no debug info".into());
                    let message = format!(
                        "GStreamer warning from {src}: {} ({debug})",
                        warning.error()
                    );
                    eprintln!("{message}");
                    crate::logging::append(message);
                }
                _ => {}
            }
        }
    };

    pipeline
        .set_state(gst::State::Null)
        .context("failed to stop pipeline")?;

    result
}

fn video_caps(fps: u32, width: Option<u32>, height: Option<u32>, d3d11: bool) -> String {
    let size = match (width, height) {
        (Some(width), Some(height)) => format!(",width={width},height={height}"),
        _ => String::new(),
    };

    if d3d11 {
        format!(
            "d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12,framerate={fps}/1{size}"
        )
    } else {
        format!("d3d11download ! videoconvert ! video/x-raw,format=NV12,framerate={fps}/1{size}")
    }
}

#[derive(Copy, Clone, Debug)]
enum SelectedEncoder {
    Nvidia,
    MediaFoundation,
    QuickSync,
    X264,
}

impl SelectedEncoder {
    fn uses_d3d11_input(self) -> bool {
        matches!(self, Self::Nvidia | Self::MediaFoundation | Self::QuickSync)
    }

    fn chain(self, bitrate: u32, fps: u32, nvidia_tuning: NvidiaTuning) -> String {
        match self {
            Self::Nvidia => nvidia_encoder_chain(bitrate, fps, nvidia_tuning),
            Self::MediaFoundation => format!(
                "mfh264enc bitrate={bitrate} max-bitrate={bitrate} gop-size={fps} bframes=0 low-latency=true rc-mode=cbr quality-vs-speed=100 \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
            Self::QuickSync => format!(
                "qsvh264enc bitrate={bitrate} gop-size={fps} b-frames=0 rc-lookahead=0 rate-control=cbr \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
            Self::X264 => format!(
                "x264enc bitrate={bitrate} speed-preset=ultrafast tune=zerolatency key-int-max={fps} bframes=0 sliced-threads=true byte-stream=true \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
        }
    }
}

fn nvidia_encoder_chain(bitrate: u32, fps: u32, tuning: NvidiaTuning) -> String {
    let tuning = resolve_nvidia_tuning(tuning);
    let vbv_buffer_size = (bitrate / fps.max(1)).max(128);
    let extra = match tuning {
        NvidiaTuning::Rtx => " spatial-aq=true temporal-aq=true aq-strength=8",
        NvidiaTuning::Gtx | NvidiaTuning::LowLatency | NvidiaTuning::Auto => "",
    };

    format!(
        "nvd3d11h264enc bitrate={bitrate} max-bitrate={bitrate} vbv-buffer-size={vbv_buffer_size} \
         gop-size={fps} bframes=0 rc-lookahead=0 zerolatency=true strict-gop=true aud=false repeat-sequence-header=true{extra} \
         ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
    )
}

fn resolve_nvidia_tuning(tuning: NvidiaTuning) -> NvidiaTuning {
    if tuning != NvidiaTuning::Auto {
        return tuning;
    }

    let Some(name) = crate::monitors::detected_nvidia_gpu_name() else {
        return NvidiaTuning::LowLatency;
    };
    let name = name.to_ascii_lowercase();
    if name.contains(" rtx") || name.contains("rtx ") || name.contains("geforce rtx") {
        NvidiaTuning::Rtx
    } else if name.contains(" gtx") || name.contains("gtx ") || name.contains("geforce gtx") {
        NvidiaTuning::Gtx
    } else {
        NvidiaTuning::LowLatency
    }
}

fn select_encoder(requested: Encoder, allow_software_encoder: bool) -> Result<SelectedEncoder> {
    match requested {
        Encoder::Auto => first_available_encoder(allow_software_encoder),
        Encoder::Nvidia => require_element("nvd3d11h264enc", SelectedEncoder::Nvidia),
        Encoder::MediaFoundation => require_element("mfh264enc", SelectedEncoder::MediaFoundation),
        Encoder::QuickSync => require_element("qsvh264enc", SelectedEncoder::QuickSync),
        Encoder::X264 => require_element("x264enc", SelectedEncoder::X264),
    }
}

fn first_available_encoder(allow_software_encoder: bool) -> Result<SelectedEncoder> {
    let hardware_encoder = [
        ("nvd3d11h264enc", SelectedEncoder::Nvidia),
        ("mfh264enc", SelectedEncoder::MediaFoundation),
        ("qsvh264enc", SelectedEncoder::QuickSync),
    ]
    .into_iter()
    .find_map(|(name, encoder)| has_element(name).then_some(encoder));

    if let Some(encoder) = hardware_encoder {
        return Ok(encoder);
    }

    if allow_software_encoder {
        return require_element("x264enc", SelectedEncoder::X264);
    }

    Err(anyhow!(
        "no supported GPU H.264 encoder found; install GStreamer Bad plugins or pass --allow-software-encoder true to permit CPU x264"
    ))
}

fn select_decoder(requested: Decoder) -> Result<&'static str> {
    match requested {
        Decoder::Auto => require_element("decodebin", "decodebin"),
        Decoder::D3d11 => require_element("d3d11h264dec", "d3d11h264dec"),
        Decoder::Avdec => require_element("avdec_h264", "avdec_h264"),
    }
}

fn select_sink(requested: Sink, fullscreen: bool) -> Result<String> {
    let d3d11_sink = if fullscreen {
        "d3d11videosink sync=false qos=true fullscreen-toggle-mode=property fullscreen=true"
    } else {
        "d3d11videosink sync=false qos=true"
    };

    match requested {
        Sink::Auto => {
            if has_element("d3d11videosink") {
                Ok(d3d11_sink.to_string())
            } else {
                Ok("autovideosink sync=false".to_string())
            }
        }
        Sink::D3d11 => require_element("d3d11videosink", d3d11_sink.to_string()),
        Sink::AutoVideo => Ok("autovideosink sync=false".to_string()),
    }
}

fn require_element<T>(name: &str, value: T) -> Result<T> {
    if has_element(name) {
        Ok(value)
    } else {
        Err(anyhow!("required GStreamer element not found: {name}"))
    }
}

pub fn has_element(name: &str) -> bool {
    gst::ElementFactory::find(name).is_some()
}

pub fn probe_elements() {
    let elements = [
        ("capture", "d3d11screencapturesrc"),
        ("gpu encode", "nvd3d11h264enc"),
        ("mf encode", "mfh264enc"),
        ("qsv encode", "qsvh264enc"),
        ("cpu encode", "x264enc"),
        ("rtp pay", "rtph264pay"),
        ("opus encode", "opusenc"),
        ("opus decode", "opusdec"),
        ("audio capture", "wasapi2src"),
        ("audio sink", "wasapi2sink"),
        ("multi udp", "multiudpsink"),
        ("rtp jitter", "rtpjitterbuffer"),
        ("rtp depay", "rtph264depay"),
        ("gpu decode", "d3d11h264dec"),
        ("cpu decode", "avdec_h264"),
        ("gpu sink", "d3d11videosink"),
    ];

    for (role, name) in elements {
        crate::console::line(format!(
            "{role:12} {name:24} {}",
            if has_element(name) { "yes" } else { "no" }
        ));
    }
}

fn ensure_positive(name: &str, value: u32) -> Result<()> {
    if value == 0 {
        Err(anyhow!("--{name} must be greater than zero"))
    } else {
        Ok(())
    }
}

fn validate_qos_dscp(value: i32) -> Result<()> {
    if value == -1 || (0..=63).contains(&value) {
        Ok(())
    } else {
        Err(anyhow!("--qos-dscp must be -1 or 0..63"))
    }
}

fn validate_audio_frame_ms(value: &str) -> Result<()> {
    match value {
        "2.5" | "5" | "10" | "20" | "40" | "60" => Ok(()),
        _ => Err(anyhow!(
            "--audio-frame-ms must be one of: 2.5, 5, 10, 20, 40, 60"
        )),
    }
}

fn multi_udp_clients(hosts: &str, port: u16) -> Result<String> {
    let clients: Vec<String> = hosts
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| {
            if has_explicit_port(host) {
                host.to_string()
            } else {
                format!("{host}:{port}")
            }
        })
        .collect();

    if clients.is_empty() {
        return Err(anyhow!("at least one receiver host is required"));
    }

    Ok(clients.join(","))
}

fn multi_udp_clients_force_port(hosts: &str, port: u16) -> Result<String> {
    let clients: Vec<String> = hosts
        .split(',')
        .map(str::trim)
        .filter(|host| !host.is_empty())
        .map(|host| {
            if let Some((address, explicit_port)) = host.rsplit_once(':') {
                if !explicit_port.is_empty()
                    && explicit_port
                        .chars()
                        .all(|character| character.is_ascii_digit())
                {
                    return format!("{address}:{port}");
                }
            }
            format!("{host}:{port}")
        })
        .collect();

    if clients.is_empty() {
        return Err(anyhow!("at least one receiver host is required"));
    }

    Ok(clients.join(","))
}

fn has_explicit_port(host: &str) -> bool {
    let Some((_, port)) = host.rsplit_once(':') else {
        return false;
    };
    !port.is_empty() && port.chars().all(|character| character.is_ascii_digit())
}

fn gst_string_literal(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
