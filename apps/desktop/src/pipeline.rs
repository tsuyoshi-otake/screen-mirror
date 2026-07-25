use anyhow::{anyhow, Context, Result};
use clap::{Args, ValueEnum};
use gstreamer as gst;
use gst::prelude::*;
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

    /// Maximum receivers to auto-connect to when host is "auto".
    #[arg(long, default_value_t = 3)]
    pub max_receivers: u32,

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

    /// RTP jitter buffer latency in milliseconds.
    #[arg(long, default_value_t = 20)]
    pub jitter_ms: u32,

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
            thread.join().map_err(|_| anyhow!("pipeline thread panicked"))??;
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
    let thread = thread::spawn(move || run_pipeline_until_stop(&description, stop_rx));
    PipelineHandle {
        stop,
        thread: Some(thread),
    }
}

pub fn build_sender_pipeline(args: &SendArgs) -> Result<String> {
    ensure_positive("fps", args.fps)?;
    ensure_positive("bitrate", args.bitrate)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("max-receivers", args.max_receivers)?;

    if args.width.is_some() != args.height.is_some() {
        return Err(anyhow!("--width and --height must be specified together"));
    }
    if args.host.trim().is_empty() || args.host.trim().eq_ignore_ascii_case("auto") {
        return Err(anyhow!(
            "sender host was not resolved; run discovery first or pass explicit --host"
        ));
    }

    let encoder = select_encoder(args.encoder)?;
    let clients = multi_udp_clients(&args.host, args.port)?;
    let source = format!(
        "d3d11screencapturesrc capture-api={} monitor-index={} show-cursor={}",
        args.capture_api,
        args.monitor_index,
        if args.no_cursor { "false" } else { "true" }
    );
    let caps = video_caps(args.fps, args.width, args.height, encoder.uses_d3d11_input());
    let encoder_chain = encoder.chain(args.bitrate, args.fps);

    Ok(format!(
        "{source} ! queue max-size-buffers=2 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {caps} \
         ! {encoder_chain} \
         ! h264parse config-interval=-1 \
         ! rtph264pay pt=96 mtu={} config-interval=-1 aggregate-mode=zero-latency \
         ! multiudpsink clients={} sync=false async=false buffer-size=2097152",
        args.mtu,
        gst_string_literal(&clients)
    ))
}

pub fn build_receiver_pipeline(args: &RecvArgs) -> Result<String> {
    ensure_positive("jitter-ms", args.jitter_ms)?;

    let decoder = select_decoder(args.decoder)?;
    let sink = select_sink(args.sink)?;

    Ok(format!(
        "udpsrc port={} buffer-size=2097152 caps=\"application/x-rtp,media=(string)video,clock-rate=(int)90000,encoding-name=(string)H264,payload=(int)96\" \
         ! rtpjitterbuffer latency={} drop-on-latency=true do-lost=true \
         ! rtph264depay \
         ! h264parse disable-passthrough=true \
         ! queue max-size-buffers=4 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {decoder} \
         ! queue max-size-buffers=2 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {sink}",
        args.port, args.jitter_ms
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
                    eprintln!(
                        "GStreamer warning from {src}: {} ({debug})",
                        warning.error()
                    );
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
        format!("d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12,framerate={fps}/1{size}")
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

    fn chain(self, bitrate: u32, fps: u32) -> String {
        match self {
            Self::Nvidia => format!(
                "nvd3d11h264enc bitrate={bitrate} gop-size={fps} bframes=0 rc-lookahead=0 zerolatency=true repeat-sequence-header=true \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
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

fn select_encoder(requested: Encoder) -> Result<SelectedEncoder> {
    match requested {
        Encoder::Auto => first_available_encoder(),
        Encoder::Nvidia => require_element("nvd3d11h264enc", SelectedEncoder::Nvidia),
        Encoder::MediaFoundation => require_element("mfh264enc", SelectedEncoder::MediaFoundation),
        Encoder::QuickSync => require_element("qsvh264enc", SelectedEncoder::QuickSync),
        Encoder::X264 => require_element("x264enc", SelectedEncoder::X264),
    }
}

fn first_available_encoder() -> Result<SelectedEncoder> {
    [
        ("nvd3d11h264enc", SelectedEncoder::Nvidia),
        ("mfh264enc", SelectedEncoder::MediaFoundation),
        ("qsvh264enc", SelectedEncoder::QuickSync),
        ("x264enc", SelectedEncoder::X264),
    ]
    .into_iter()
    .find_map(|(name, encoder)| has_element(name).then_some(encoder))
    .ok_or_else(|| anyhow!("no supported H.264 encoder found; install GStreamer Bad/Ugly plugins"))
}

fn select_decoder(requested: Decoder) -> Result<&'static str> {
    match requested {
        Decoder::Auto => {
            if has_element("d3d11h264dec") {
                Ok("d3d11h264dec")
            } else if has_element("avdec_h264") {
                Ok("avdec_h264")
            } else {
                Err(anyhow!(
                    "no supported H.264 decoder found; install GStreamer Bad/Libav plugins"
                ))
            }
        }
        Decoder::D3d11 => require_element("d3d11h264dec", "d3d11h264dec"),
        Decoder::Avdec => require_element("avdec_h264", "avdec_h264"),
    }
}

fn select_sink(requested: Sink) -> Result<&'static str> {
    match requested {
        Sink::Auto => {
            if has_element("d3d11videosink") {
                Ok("d3d11videosink sync=false qos=true")
            } else {
                Ok("autovideosink sync=false")
            }
        }
        Sink::D3d11 => require_element("d3d11videosink", "d3d11videosink sync=false qos=true"),
        Sink::AutoVideo => Ok("autovideosink sync=false"),
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
        ("multi udp", "multiudpsink"),
        ("rtp jitter", "rtpjitterbuffer"),
        ("rtp depay", "rtph264depay"),
        ("gpu decode", "d3d11h264dec"),
        ("cpu decode", "avdec_h264"),
        ("gpu sink", "d3d11videosink"),
    ];

    for (role, name) in elements {
        println!(
            "{role:12} {name:24} {}",
            if has_element(name) { "yes" } else { "no" }
        );
    }
}

fn ensure_positive(name: &str, value: u32) -> Result<()> {
    if value == 0 {
        Err(anyhow!("--{name} must be greater than zero"))
    } else {
        Ok(())
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

fn has_explicit_port(host: &str) -> bool {
    let Some((_, port)) = host.rsplit_once(':') else {
        return false;
    };
    !port.is_empty() && port.chars().all(|character| character.is_ascii_digit())
}

fn gst_string_literal(value: &str) -> String {
    format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    )
}
