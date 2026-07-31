use anyhow::{anyhow, Context, Result};
use clap::{ArgAction, Args, ValueEnum};
use gst::prelude::*;
use gstreamer as gst;
use sm_core::discovery::DEFAULT_PIN;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const SENDER_VIDEO_ENCODER_NAME: &str = "sender_video_encoder";
const SENDER_RTP_PAY_NAME: &str = "sender_rtp_pay";
const FORCE_KEY_UNIT_INTERVAL: Duration = Duration::from_secs(1);
/// How long the receiver waits for video packets before deciding the stream is gone.
///
/// Wi-Fi roaming, VPN tunnels, and radio power saving all produce gaps of a few seconds that the
/// stream comes back from on its own: the sender keeps streaming for its whole receiver-loss grace
/// and forces a keyframe every second, so playback resumes within a second of the packets returning.
/// Giving up too early turns that hiccup into a closed fullscreen window and a listener restart,
/// which is far more disruptive than the gap it reacts to.
const RECEIVER_PACKET_TIMEOUT: Duration = Duration::from_secs(8);
const RECEIVER_VIDEO_DECODER_NAME: &str = "receiver_video_decoder";
const RECEIVER_VIDEO_SINK_NAME: &str = "receiver_video_sink";
const RECEIVER_VIDEO_JITTER_NAME: &str = "receiver_video_jitter";
/// How often the receiver records what the stream is actually doing.
///
/// Long enough that one slow frame does not read as a collapse, short enough that a stutter the user
/// noticed is inside the window they will point at in the log.
const RECEIVER_STREAM_REPORT_INTERVAL: Duration = Duration::from_secs(5);

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
    #[arg(long, default_value_t = 30)]
    pub fps: u32,

    /// Target bitrate in kbit/sec.
    #[arg(long, default_value_t = 8_000)]
    pub bitrate: u32,

    /// RTP MTU. 1200 is safe for Wi-Fi and VPN-ish paths.
    #[arg(long, default_value_t = 1200)]
    pub mtu: u32,

    /// UDP kernel send buffer size in bytes.
    #[arg(long, default_value_t = 1024 * 1024)]
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

    /// GPU to encode on: "auto", a DXGI adapter index, or part of the adapter name.
    #[arg(long, default_value = crate::gpu::AUTO)]
    pub gpu: String,

    /// Capture API. dxgi is lowest overhead; wgc can behave better with modern Windows/window capture.
    #[arg(long, value_enum, default_value_t = CaptureApi::Dxgi)]
    pub capture_api: CaptureApi,

    /// Keep captured frames in D3D11 memory through hardware encoding when supported.
    #[arg(long, default_value_t = true, action = ArgAction::Set)]
    pub zero_copy: bool,

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
    #[arg(long, default_value_t = 10)]
    pub audio_jitter_ms: u32,

    /// Four-digit PIN advertised for LAN discovery pairing.
    #[arg(long, default_value = DEFAULT_PIN)]
    pub pin: String,

    /// RTP jitter buffer latency in milliseconds.
    #[arg(long, default_value_t = 15)]
    pub jitter_ms: u32,

    /// UDP kernel receive buffer size in bytes.
    #[arg(long, default_value_t = 1024 * 1024)]
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

    /// Decoder to use. auto prefers a matching D3D12 route on modern Intel, then D3D11 GPU decode,
    /// and retries on the bundled software decoder when hardware decode rejects the stream.
    #[arg(long, value_enum, default_value_t = Decoder::Auto)]
    pub decoder: Decoder,

    /// Video sink to use. auto pairs D3D12 on capable Intel GPUs, then prefers D3D11 rendering.
    #[arg(long, value_enum, default_value_t = Sink::Auto)]
    pub sink: Sink,

    /// GPU to decode and render on: "auto", a DXGI adapter index, or part of the adapter name.
    #[arg(long, default_value = crate::gpu::AUTO)]
    pub gpu: String,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
pub enum Encoder {
    Auto,
    Nvidia,
    Amf,
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
    Software,
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

#[derive(Clone, Debug)]
pub struct ReceiverPipelinePlan {
    primary: String,
    /// Ordered retry routes, most capable first. A hardware decoder rejects a stream that exceeds
    /// its DXVA limits before any frame reaches it, so every route after the first exists to keep
    /// the receiver showing a picture instead of stopping on `not-negotiated`.
    fallbacks: Vec<ReceiverFallbackRoute>,
    /// Largest frame the primary decoder accepts, announced to senders so they can scale down
    /// before encoding instead of pushing the receiver onto a software fallback.
    decode_limits: Option<(u32, u32)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiverFallbackRoute {
    label: &'static str,
    description: String,
}

impl ReceiverPipelinePlan {
    pub fn primary(&self) -> &str {
        &self.primary
    }

    pub fn decode_limits(&self) -> Option<(u32, u32)> {
        self.decode_limits
    }

    fn append_pipeline(&mut self, description: &str) {
        self.primary.push(' ');
        self.primary.push_str(description);
        for fallback in self.fallbacks.iter_mut() {
            fallback.description.push(' ');
            fallback.description.push_str(description);
        }
    }
}

struct SenderKeyUnitRequester {
    request_src: gst::Pad,
    schedule: KeyUnitSchedule,
    last_accepted: Option<bool>,
}

struct KeyUnitSchedule {
    next_request: Instant,
    count: u32,
}

impl KeyUnitSchedule {
    fn new(now: Instant) -> Self {
        Self {
            next_request: now,
            count: 0,
        }
    }

    fn take_due(&mut self, now: Instant) -> Option<u32> {
        if now < self.next_request {
            return None;
        }

        let count = self.count;
        self.count = self.count.wrapping_add(1);
        self.next_request = now + FORCE_KEY_UNIT_INTERVAL;
        Some(count)
    }
}

impl SenderKeyUnitRequester {
    fn attach(pipeline: &gst::Pipeline) -> Option<Self> {
        let payloader = pipeline.by_name(SENDER_RTP_PAY_NAME)?;
        let Some(request_src) = payloader.static_pad("src") else {
            crate::logging::append(
                "sender force-key-unit requests disabled: RTP payloader has no static src pad",
            );
            return None;
        };

        Some(Self {
            request_src,
            schedule: KeyUnitSchedule::new(Instant::now()),
            last_accepted: None,
        })
    }

    fn request_if_due(&mut self, now: Instant) -> bool {
        let Some(count) = self.schedule.take_due(now) else {
            return false;
        };

        let accepted = self
            .request_src
            .send_event(upstream_force_key_unit_event(count));
        if self.last_accepted != Some(accepted) {
            crate::logging::append(if accepted {
                format!(
                    "sender force-key-unit requests active: interval={}ms all-headers=true",
                    FORCE_KEY_UNIT_INTERVAL.as_millis()
                )
            } else {
                "sender force-key-unit request rejected upstream; retrying".to_string()
            });
        }
        self.last_accepted = Some(accepted);
        true
    }
}

/// Builds the stable `GstForceKeyUnit` custom event without adding a link-time dependency on
/// gstvideo. This mirrors `gst_video_event_new_upstream_force_key_unit()` with
/// `GST_CLOCK_TIME_NONE`, so an immediate keyframe with all headers is requested.
fn upstream_force_key_unit_event(count: u32) -> gst::Event {
    gst::event::CustomUpstream::new(
        gst::Structure::builder("GstForceKeyUnit")
            .field("running-time", gst::ClockTime::NONE)
            .field("all-headers", true)
            .field("count", count)
            .build(),
    )
}

impl PipelineHandle {
    pub fn is_finished(&self) -> bool {
        self.thread
            .as_ref()
            .map(|thread| thread.is_finished())
            .unwrap_or(true)
    }

    pub fn finish(mut self) -> Result<()> {
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| anyhow!("pipeline thread panicked"))??;
        }
        Ok(())
    }

    pub fn stop(self) -> Result<()> {
        let _ = self.stop.send(());
        self.finish()
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
        let result = run_pipeline_until_stop(&description, &stop_rx);
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

pub fn spawn_receiver_pipeline(plan: ReceiverPipelinePlan) -> PipelineHandle {
    let (stop, stop_rx) = mpsc::channel();
    let thread = thread::spawn(move || {
        let result = run_receiver_pipeline_plan_until_stop(plan, &stop_rx);
        if let Err(error) = &result {
            crate::logging::append(format!("receiver pipeline failed: {error:#}"));
        }
        result
    });
    PipelineHandle {
        stop,
        thread: Some(thread),
    }
}

pub fn build_sender_pipeline(args: &SendArgs) -> Result<String> {
    build_sender_pipeline_for(args, None)
}

pub fn build_sender_pipeline_for(
    args: &SendArgs,
    target: Option<&crate::monitors::DisplayMonitor>,
) -> Result<String> {
    let video = build_sender_video_pipeline_for(args, target)?;
    if !args.audio_enabled {
        return Ok(video);
    }

    Ok(format!("{video} {}", build_sender_audio_pipeline(args)?))
}

/// Same pipeline, but pinned to one capture target so each receiver can get its own display.
pub fn build_sender_video_pipeline_for(
    args: &SendArgs,
    target: Option<&crate::monitors::DisplayMonitor>,
) -> Result<String> {
    ensure_positive("fps", args.fps)?;
    ensure_positive("bitrate", args.bitrate)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("max-receivers", args.max_receivers)?;
    validate_qos_dscp(args.qos_dscp)?;

    if args.width.is_some() != args.height.is_some() {
        return Err(anyhow!("--width and --height must be specified together"));
    }
    if args.host.trim().is_empty() || args.host.trim().eq_ignore_ascii_case("auto") {
        return Err(anyhow!(
            "sender host was not resolved; run discovery first or pass explicit --host"
        ));
    }

    let clients = multi_udp_clients(&args.host, args.port)?;
    let show_cursor = if args.no_cursor { "false" } else { "true" };
    let mut capture_target: Option<crate::monitors::DisplayMonitor> = target.cloned();
    let source = if let Some(target) = target {
        crate::logging::append(format!(
            "sender assigned capture target: {}",
            target.summary()
        ));
        capture_source_for_target(args.capture_api, show_cursor, target)?
    } else if args.prefer_virtual_display && args.monitor_index < 0 {
        match crate::monitors::preferred_virtual_capture_target() {
            Some(target) => {
                crate::logging::append(format!(
                    "sender preferred virtual display: {}",
                    target.summary()
                ));
                if let Some(monitor_handle) = target.monitor_handle {
                    crate::logging::append(format!(
                        "sender capture monitor-handle selected: {monitor_handle}; capture-api={}; adapter={}",
                        args.capture_api, target.adapter_name
                    ));
                } else if !target.bundled_virtual_display {
                    let monitor_index = target.capture_index.unwrap_or(-1);
                    crate::logging::append(format!(
                        "sender fallback capture monitor-index selected: {monitor_index}; capture-api={}; adapter={}",
                        args.capture_api, target.adapter_name
                    ));
                }
                let source = capture_source_for_target(args.capture_api, show_cursor, &target)?;
                capture_target = Some(target);
                source
            }
            None => {
                crate::logging::append("sender preferred virtual display: not found");
                if args.enable_virtual_display {
                    return Err(anyhow!(
                        "bundled virtual display is enabled but no capture-ready virtual target was found; refusing physical-display fallback"
                    ));
                }
                format!(
                    "d3d11screencapturesrc capture-api={} monitor-index={} show-cursor={}",
                    args.capture_api, args.monitor_index, show_cursor
                )
            }
        }
    } else {
        let monitor_index = crate::monitors::resolve_capture_monitor_index(
            args.monitor_index,
            args.prefer_virtual_display,
        );
        format!(
            "d3d11screencapturesrc capture-api={} monitor-index={} show-cursor={}",
            args.capture_api, monitor_index, show_cursor
        )
    };
    crate::logging::append(format!("sender pipeline source: {source}"));
    let capture_adapter = capture_adapter(capture_target.as_ref(), args.monitor_index);
    let gpu = sender_gpu(&args.gpu, capture_adapter.as_ref());
    if let Some(gpu) = gpu.as_ref() {
        crate::logging::append(format!("sender GPU selected: {}", gpu.summary()));
    }
    let encoder = select_encoder(args.encoder, args.allow_software_encoder, gpu.as_ref())?;
    let encoder_accepts_d3d11 = encoder.supports_d3d11_input();
    // Frames captured on one GPU cannot stay in D3D11 memory while another GPU encodes them, so
    // that combination goes through system memory and lets the encoder upload on its own device.
    let same_gpu_as_capture = match encoder.adapter_luid {
        Some(encoder_luid) => {
            capture_adapter.as_ref().map(|adapter| adapter.luid) == Some(encoder_luid)
        }
        None => true,
    };
    let use_d3d11_memory = args.zero_copy && encoder_accepts_d3d11 && same_gpu_as_capture;
    if args.zero_copy && !encoder_accepts_d3d11 {
        crate::logging::append(format!(
            "sender encoder={} does not advertise D3D11 input; falling back to system-memory frames",
            encoder.name()
        ));
    } else if args.zero_copy && !same_gpu_as_capture {
        crate::logging::append(format!(
            "sender encoder={} does not run on the capture GPU; falling back to system-memory frames",
            encoder.name()
        ));
    }
    crate::logging::append(format!(
        "sender encoder={} frame-memory={} fps={} bitrate={}kbit/s udp-buffer={}bytes",
        encoder.name(),
        if use_d3d11_memory { "D3D11" } else { "system" },
        args.fps,
        args.bitrate,
        args.udp_buffer_size
    ));
    let caps = video_caps(args.fps, args.width, args.height, use_d3d11_memory);
    let encoder_chain = encoder.chain(args.bitrate, args.fps, args.nvidia_tuning);

    let video = format!(
        "{source} ! queue max-size-buffers=1 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {caps} \
         ! {encoder_chain} \
         ! h264parse config-interval=-1 \
         ! rtph264pay name={SENDER_RTP_PAY_NAME} pt=96 mtu={} config-interval=-1 aggregate-mode=zero-latency \
         ! multiudpsink clients={} sync=false async=false buffer-size={} qos-dscp={} send-duplicates=false ttl=1",
        args.mtu,
        gst_string_literal(&clients),
        args.udp_buffer_size,
        args.qos_dscp
    );

    Ok(video)
}

pub fn build_sender_audio_pipeline(args: &SendArgs) -> Result<String> {
    ensure_positive("audio-bitrate", args.audio_bitrate)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    validate_qos_dscp(args.qos_dscp)?;
    validate_audio_frame_ms(&args.audio_frame_ms)?;
    if args.host.trim().is_empty() || args.host.trim().eq_ignore_ascii_case("auto") {
        return Err(anyhow!(
            "sender host was not resolved; run discovery first or pass explicit --host"
        ));
    }
    build_sender_audio_chain(args)
}

fn capture_source_for_target(
    capture_api: CaptureApi,
    show_cursor: &str,
    target: &crate::monitors::DisplayMonitor,
) -> Result<String> {
    if let Some(monitor_handle) = target.monitor_handle {
        return Ok(format!(
            "d3d11screencapturesrc capture-api={capture_api} monitor-handle={monitor_handle} show-cursor={show_cursor}"
        ));
    }

    if target.bundled_virtual_display {
        return Err(anyhow!(
            "bundled virtual display {} has no stable monitor handle; refusing ambiguous monitor-index capture",
            target.adapter_name
        ));
    }

    let monitor_index = target.capture_index.unwrap_or(-1);
    Ok(format!(
        "d3d11screencapturesrc capture-api={capture_api} monitor-index={monitor_index} show-cursor={show_cursor}"
    ))
}

/// The GPU DXGI desktop duplication captures on, which is always the GPU that drives the captured
/// monitor. `None` when the capture target cannot be tied to a monitor.
/// Resolves the GPU the sender encodes on. `auto` follows the adapter that owns the capture target
/// so a multi-GPU machine does not encode on an adapter the frames never touch, which would force
/// every frame through system memory.
fn sender_gpu(
    selection: &str,
    capture_adapter: Option<&crate::gpu::GpuAdapter>,
) -> Option<crate::gpu::GpuAdapter> {
    match crate::gpu::resolve(selection) {
        Some(gpu) => Some(gpu),
        None => {
            if let Some(adapter) = capture_adapter {
                crate::logging::append(format!(
                    "sender automatic GPU selected from the capture display: {}",
                    adapter.summary()
                ));
            }
            capture_adapter.cloned()
        }
    }
}

fn capture_adapter(
    target: Option<&crate::monitors::DisplayMonitor>,
    monitor_index: i32,
) -> Option<crate::gpu::GpuAdapter> {
    if let Some(target) = target {
        return target
            .monitor_handle
            .and_then(crate::gpu::adapter_for_monitor_handle)
            .or_else(|| crate::gpu::adapter_for_display_device_name(&target.adapter_name));
    }

    let monitors = crate::monitors::enumerate_monitors();
    capture_monitor_for_index(&monitors, monitor_index).and_then(|monitor| {
        monitor
            .monitor_handle
            .and_then(crate::gpu::adapter_for_monitor_handle)
            .or_else(|| crate::gpu::adapter_for_display_device_name(&monitor.adapter_name))
    })
}

fn capture_monitor_for_index(
    monitors: &[crate::monitors::DisplayMonitor],
    monitor_index: i32,
) -> Option<&crate::monitors::DisplayMonitor> {
    if monitor_index >= 0 {
        monitors
            .iter()
            .find(|monitor| monitor.capture_index == Some(monitor_index))
    } else {
        monitors.iter().find(|monitor| monitor.primary)
    }
}

pub fn build_receiver_pipeline(args: &RecvArgs) -> Result<String> {
    Ok(build_receiver_pipeline_plan(args)?.primary)
}

pub fn build_receiver_pipeline_plan(args: &RecvArgs) -> Result<ReceiverPipelinePlan> {
    let mut plan = build_receiver_video_plan(args)?;
    if args.audio_enabled {
        plan.append_pipeline(&build_receiver_audio_pipeline(args)?);
    }
    Ok(plan)
}

pub fn build_receiver_video_plan(args: &RecvArgs) -> Result<ReceiverPipelinePlan> {
    let gpu = crate::gpu::resolve_receiver(&args.gpu);
    if let Some(gpu) = gpu.as_ref() {
        crate::logging::append(format!("receiver GPU selected: {}", gpu.summary()));
    }
    let (primary, route) = build_receiver_video_pipeline_for(args, gpu.as_ref(), true, true)?;
    let decode_limits = decoder_frame_limits(&route.decoder.factory);
    match decode_limits {
        Some((width, height)) => crate::logging::append(format!(
            "receiver decoder {} accepts up to {width}x{height}; announcing that limit to senders",
            route.decoder.factory
        )),
        None => crate::logging::append(format!(
            "receiver decoder {} advertises no frame-size limit; senders will not scale for it",
            route.decoder.factory
        )),
    }
    let mut fallbacks = Vec::new();

    if route.uses_d3d12() {
        match build_receiver_video_pipeline_for(args, gpu.as_ref(), false, false) {
            Ok((description, _)) => fallbacks.push(ReceiverFallbackRoute {
                label: "D3D11/QSV",
                description,
            }),
            Err(error) => crate::logging::append(format!(
                "receiver D3D12 route has no compatible D3D11/QSV fallback: {error:#}"
            )),
        }
    }

    match build_receiver_software_video_pipeline(args, gpu.as_ref()) {
        Ok(Some(description)) => fallbacks.push(ReceiverFallbackRoute {
            label: "software",
            description,
        }),
        Ok(None) => {}
        Err(error) => crate::logging::append(format!(
            "receiver has no software H.264 fallback route: {error:#}"
        )),
    }

    Ok(ReceiverPipelinePlan {
        primary,
        fallbacks,
        decode_limits,
    })
}

/// Hardware decoders advertise fixed caps: Intel HD Graphics 4000 tops out at 1920x1920 and no
/// DXVA decoder accepts 4:4:4. A stream outside those caps fails to negotiate before the decoder
/// sees a frame, which used to leave the receiver with no picture and no retry. Software decode
/// has no such limit, so it is kept as the last route for every GPU.
fn build_receiver_software_video_pipeline(
    args: &RecvArgs,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Result<Option<String>> {
    // An explicitly requested software decoder is already the primary route.
    if matches!(args.decoder, Decoder::Avdec | Decoder::Software) {
        return Ok(None);
    }
    let Some(decoder) = software_decoder() else {
        return Ok(None);
    };
    let route = ReceiverVideoRoute {
        decoder,
        sink: select_sink(args.sink, args.fullscreen, gpu)?,
    };
    Ok(Some(receiver_video_pipeline(args, &route)?))
}

fn build_receiver_video_pipeline_for(
    args: &RecvArgs,
    gpu: Option<&crate::gpu::GpuAdapter>,
    allow_modern_intel_d3d12: bool,
    log_route: bool,
) -> Result<(String, ReceiverVideoRoute)> {
    let route = select_receiver_route(
        args.decoder,
        args.sink,
        args.fullscreen,
        gpu,
        allow_modern_intel_d3d12,
    )?;
    if log_route {
        let profile = receiver_gpu_profile(gpu);
        crate::logging::append(format!(
            "receiver profile={} adapter={} decoder={} decoder-luid={} memory={} sink={}",
            profile.route_label(route.decoder.memory),
            gpu.map(crate::gpu::GpuAdapter::summary)
                .unwrap_or_else(|| "GStreamer default".to_string()),
            route.decoder.factory,
            route
                .decoder
                .adapter_luid
                .map(|luid| luid.to_string())
                .unwrap_or_else(|| "default/unknown".to_string()),
            route.decoder.memory_label_for(&route.sink),
            route.sink
        ));
    }

    Ok((receiver_video_pipeline(args, &route)?, route))
}

/// Largest frame the decoder advertises on its sink pad.
///
/// GStreamer's D3D11/D3D12/QSV decoders probe DXVA when they register and bake the answer into
/// their pad templates, so an Intel HD 4000 reports `width=[1,1920]` where an RTX 4060 Ti reports
/// far more. A decoder that names no bound — `decodebin`, and the software decoders — is reported
/// as unlimited, which is what senders assume for peers that announce nothing.
fn decoder_frame_limits(factory: &str) -> Option<(u32, u32)> {
    let factory = gst::ElementFactory::find(factory)?;
    let mut limits: Option<(u32, u32)> = None;
    for template in factory.static_pad_templates() {
        if template.direction() != gst::PadDirection::Sink {
            continue;
        }
        let caps = template.caps();
        if caps.is_any() {
            continue;
        }
        for structure in caps.iter() {
            if structure.name() != "video/x-h264" {
                continue;
            }
            let Some(width) = maximum_dimension(structure, "width") else {
                continue;
            };
            let Some(height) = maximum_dimension(structure, "height") else {
                continue;
            };
            // A decoder can list several H.264 caps; the widest one is what it accepts.
            limits = Some(match limits {
                Some((known_width, known_height)) => {
                    (known_width.max(width), known_height.max(height))
                }
                None => (width, height),
            });
        }
    }
    limits
}

/// Reads a caps dimension that hardware decoders write as a range and software ones sometimes
/// write as a single value.
fn maximum_dimension(structure: &gst::StructureRef, field: &str) -> Option<u32> {
    if let Ok(range) = structure.get::<gst::IntRange<i32>>(field) {
        return u32::try_from(range.max()).ok();
    }
    structure
        .get::<i32>(field)
        .ok()
        .and_then(|value| u32::try_from(value).ok())
}

fn receiver_video_pipeline(args: &RecvArgs, route: &ReceiverVideoRoute) -> Result<String> {
    ensure_positive("jitter-ms", args.jitter_ms)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("jitter-faststart-packets", args.jitter_faststart_packets)?;
    ensure_positive("jitter-max-dropout-ms", args.jitter_max_dropout_ms)?;
    ensure_positive("jitter-max-misorder-ms", args.jitter_max_misorder_ms)?;

    // Keep the decoder and renderer in one graphics API. Explicit compatibility modes are left
    // negotiated so GStreamer can choose a system-memory bridge instead of crossing GPU textures.
    let output_caps = route.decoder.output_caps_for(&route.sink);
    let sink = &route.sink;

    Ok(format!(
        "udpsrc name=receiver_video_src port={} buffer-size={} mtu={} retrieve-sender-address=false timeout=0 caps=\"application/x-rtp,media=(string)video,clock-rate=(int)90000,encoding-name=(string)H264,payload=(int)96,packetization-mode=(string)1\" \
         ! rtpjitterbuffer name={RECEIVER_VIDEO_JITTER_NAME} latency={} drop-on-latency=true do-lost=true faststart-min-packets={} max-dropout-time={} max-misorder-time={} \
         ! rtph264depay wait-for-keyframe=true \
         ! h264parse disable-passthrough=true \
         ! queue max-size-buffers=4 max-size-time=0 max-size-bytes=0 \
         ! {}{} \
         ! queue max-size-buffers=1 max-size-time=0 max-size-bytes=0 leaky=downstream \
         ! {sink}",
        args.port,
        args.udp_buffer_size,
        args.mtu,
        args.jitter_ms,
        args.jitter_faststart_packets,
        args.jitter_max_dropout_ms,
        args.jitter_max_misorder_ms,
        route.decoder.pipeline_element(),
        output_caps,
    ))
}

pub fn build_receiver_audio_pipeline(args: &RecvArgs) -> Result<String> {
    ensure_positive("audio-jitter-ms", args.audio_jitter_ms)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("mtu", args.mtu)?;
    build_receiver_audio_chain(args)
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
        "wasapi2src loopback=true low-latency=true provide-clock=false \
         ! queue max-size-buffers=2 max-size-time=20000000 max-size-bytes=0 leaky=downstream \
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
        "wasapi2sink low-latency=true sync=false async=false"
    } else {
        "autoaudiosink sync=false"
    };
    Ok(format!(
        "udpsrc port={} buffer-size={} mtu={} retrieve-sender-address=false caps=\"application/x-rtp,media=(string)audio,clock-rate=(int)48000,encoding-name=(string)OPUS,payload=(int)97\" \
         ! queue max-size-buffers=4 max-size-time=20000000 max-size-bytes=0 leaky=downstream \
         ! rtpjitterbuffer latency={} drop-on-latency=true do-lost=true faststart-min-packets=2 \
         ! rtpopusdepay ! opusdec plc=true \
         ! queue max-size-buffers=2 max-size-time=10000000 max-size-bytes=0 leaky=downstream \
         ! audioconvert ! audioresample ! {sink}",
        args.audio_port, args.udp_buffer_size, args.mtu, args.audio_jitter_ms
    ))
}

pub fn run_pipeline(description: &str) -> Result<()> {
    let (_stop, stop_rx) = mpsc::channel();
    run_pipeline_until_stop(description, &stop_rx)
}

pub fn run_receiver_pipeline_plan(plan: ReceiverPipelinePlan) -> Result<()> {
    let (_stop, stop_rx) = mpsc::channel();
    run_receiver_pipeline_plan_until_stop(plan, &stop_rx)
}

fn run_receiver_pipeline_plan_until_stop(
    plan: ReceiverPipelinePlan,
    stop_rx: &mpsc::Receiver<()>,
) -> Result<()> {
    let ReceiverPipelinePlan {
        primary, fallbacks, ..
    } = plan;
    let primary_error = match run_pipeline_until_stop(&primary, stop_rx) {
        Err(error) => error,
        result => return result,
    };

    let mut last_error = primary_error;
    for (index, route) in fallbacks.iter().enumerate() {
        if stop_rx.try_recv().is_ok() {
            return Ok(());
        }

        crate::logging::append(format!(
            "receiver route failed; retrying on the {} route: {last_error:#}",
            route.label
        ));
        crate::logging::append(format!(
            "receiver fallback pipeline ({}): {}",
            route.label, route.description
        ));
        match run_pipeline_until_stop(&route.description, stop_rx) {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                if index + 1 == fallbacks.len() {
                    return Err(last_error).with_context(|| {
                        format!(
                            "every receiver decode route failed (last: {} route)",
                            route.label
                        )
                    });
                }
            }
        }
    }

    Err(last_error)
}

fn run_pipeline_until_stop(description: &str, stop_rx: &mpsc::Receiver<()>) -> Result<()> {
    let element = gst::parse::launch(description).context("failed to parse GStreamer pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("pipeline description did not create a GstPipeline"))?;
    let bus = pipeline.bus().context("pipeline has no bus")?;
    arm_receiver_timeout_after_first_packet(&pipeline);

    pipeline
        .set_state(gst::State::Playing)
        .context("failed to set pipeline to Playing")?;
    let mut key_unit_requester = SenderKeyUnitRequester::attach(&pipeline);
    let mut stream_report = ReceiverStreamReport::attach(&pipeline, Instant::now());
    let mut receiver_runtime_logged = pipeline
        .by_name(RECEIVER_VIDEO_DECODER_NAME)
        .is_some()
        .then_some(false);

    let result = loop {
        if stop_rx.try_recv().is_ok() {
            break Ok(());
        }

        if let Some(requester) = key_unit_requester.as_mut() {
            requester.request_if_due(Instant::now());
        }

        if let Some(report) = stream_report.as_mut() {
            report.report_if_due(Instant::now());
        }

        if let Some(logged) = receiver_runtime_logged.as_mut() {
            if !*logged {
                *logged = log_receiver_runtime_path(&pipeline);
            }
        }

        if let Some(message) = bus.timed_pop(gst::ClockTime::from_mseconds(100)) {
            use gst::MessageView;
            match message.view() {
                MessageView::Eos(..) => break Ok(()),
                MessageView::Element(element) => {
                    if element
                        .structure()
                        .map(|structure| structure.name() == "GstUDPSrcTimeout")
                        .unwrap_or(false)
                    {
                        crate::logging::append("receiver timed out waiting for video packets");
                        break Ok(());
                    }
                }
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

/// Records how many frames the receiver decoded and how many of them reached the screen.
///
/// "The framerate is low" has three different causes that look identical on screen, and telling them
/// apart by inspection is guesswork: the sender may never have sent 30 frames, the decoder may not
/// keep up with them, or frames may be decoded and then dropped before they are drawn. Each of those
/// wants an opposite fix, so the receiver counts all three and the RTP packets the jitter buffer
/// discarded, which is what says whether the frames that never arrived were late rather than absent.
struct ReceiverStreamReport {
    decoded: Option<Arc<AtomicU64>>,
    displayed: Arc<AtomicU64>,
    jitter: Option<gst::Element>,
    jitter_total: JitterCounts,
    window_started: Instant,
}

impl ReceiverStreamReport {
    /// Attaches to a receiver pipeline, and to nothing else: a sender has no video sink to count at.
    fn attach(pipeline: &gst::Pipeline, now: Instant) -> Option<Self> {
        let sink = pipeline.by_name(RECEIVER_VIDEO_SINK_NAME)?;
        let displayed = count_buffers_on(&sink, "sink")?;
        // `decodebin` exposes no static source pad, so its frames are counted at the sink only.
        let decoded = pipeline
            .by_name(RECEIVER_VIDEO_DECODER_NAME)
            .and_then(|decoder| count_buffers_on(&decoder, "src"));
        let jitter = pipeline
            .by_name(RECEIVER_VIDEO_JITTER_NAME)
            .filter(|jitter| jitter.find_property("stats").is_some());
        let jitter_total = jitter.as_ref().map(jitter_counts).unwrap_or_default();
        Some(Self {
            decoded,
            displayed,
            jitter,
            jitter_total,
            window_started: now,
        })
    }

    fn report_if_due(&mut self, now: Instant) {
        let window = now.duration_since(self.window_started);
        if window < RECEIVER_STREAM_REPORT_INTERVAL {
            return;
        }
        self.window_started = now;

        let jitter = self.jitter.as_ref().map(jitter_counts);
        let since_last_report = jitter.map(|counts| counts.since(self.jitter_total));
        if let Some(counts) = jitter {
            self.jitter_total = counts;
        }

        crate::logging::append(stream_report_line(
            window,
            take_count(&self.displayed),
            self.decoded.as_ref().map(take_count),
            since_last_report,
        ));
    }
}

fn count_buffers_on(element: &gst::Element, pad: &str) -> Option<Arc<AtomicU64>> {
    let pad = element.static_pad(pad)?;
    let count = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&count);
    pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
        counted.fetch_add(1, Ordering::Relaxed);
        gst::PadProbeReturn::Ok
    })?;
    Some(count)
}

fn take_count(count: &Arc<AtomicU64>) -> u64 {
    count.swap(0, Ordering::Relaxed)
}

/// RTP packet counters, which `rtpjitterbuffer` reports as running totals.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct JitterCounts {
    pushed: u64,
    lost: u64,
    late: u64,
    duplicates: u64,
}

impl JitterCounts {
    fn since(self, earlier: Self) -> Self {
        Self {
            pushed: self.pushed.saturating_sub(earlier.pushed),
            lost: self.lost.saturating_sub(earlier.lost),
            late: self.late.saturating_sub(earlier.late),
            duplicates: self.duplicates.saturating_sub(earlier.duplicates),
        }
    }
}

fn jitter_counts(jitter: &gst::Element) -> JitterCounts {
    let Ok(stats) = jitter.property_value("stats").get::<gst::Structure>() else {
        return JitterCounts::default();
    };
    let count = |field| stats.get::<u64>(field).unwrap_or_default();
    JitterCounts {
        pushed: count("num-pushed"),
        lost: count("num-lost"),
        late: count("num-late"),
        duplicates: count("num-duplicates"),
    }
}

/// Frame counts are reported as both a count and a rate: the rate is what the user is describing,
/// and the count is what stays meaningful when a report window is stretched by a stalled pipeline.
fn stream_report_line(
    window: Duration,
    displayed: u64,
    decoded: Option<u64>,
    jitter: Option<JitterCounts>,
) -> String {
    let seconds = window.as_secs_f64();
    let rate = |count: u64| {
        if seconds > 0.0 {
            format!("{:.1} fps", count as f64 / seconds)
        } else {
            "no window".to_string()
        }
    };
    let mut line = format!("receiver stream {seconds:.1}s:");
    if let Some(decoded) = decoded {
        line.push_str(&format!(" decoded={decoded} ({})", rate(decoded)));
    }
    line.push_str(&format!(" displayed={displayed} ({})", rate(displayed)));
    if let Some(jitter) = jitter {
        line.push_str(&format!(
            " rtp pushed={} lost={} late={} duplicates={}",
            jitter.pushed, jitter.lost, jitter.late, jitter.duplicates
        ));
    }
    line
}

fn arm_receiver_timeout_after_first_packet(pipeline: &gst::Pipeline) {
    let Some(source) = pipeline.by_name("receiver_video_src") else {
        return;
    };
    let Some(pad) = source.static_pad("src") else {
        return;
    };
    let source_for_probe = source.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
        source_for_probe.set_property("timeout", RECEIVER_PACKET_TIMEOUT.as_nanos() as u64);
        crate::logging::append("receiver video packets started; disconnect timeout armed");
        gst::PadProbeReturn::Remove
    });
}

/// Waits until decoding has negotiated caps, then records the element GStreamer actually chose.
/// This is especially important for `decodebin`, whose configured name is not its final decoder.
fn log_receiver_runtime_path(pipeline: &gst::Pipeline) -> bool {
    let decoder = pipeline
        .iterate_recurse()
        .into_iter()
        .filter_map(Result::ok)
        .find(|element| {
            element.factory().is_some_and(|factory| {
                let name = factory.name();
                name.contains("h264") && (name.ends_with("dec") || name == "avdec_h264")
            })
        });
    let Some(decoder) = decoder else {
        return false;
    };
    let Some(src_pad) = decoder.static_pad("src") else {
        return false;
    };
    let Some(caps) = src_pad.current_caps() else {
        return false;
    };
    let decoder_factory = decoder
        .factory()
        .map(|factory| factory.name().to_string())
        .unwrap_or_else(|| decoder.name().to_string());
    let decoder_luid = element_numeric_property(&decoder, "adapter-luid")
        .unwrap_or_else(|| "default/unknown".to_string());
    let sink_element = pipeline.by_name(RECEIVER_VIDEO_SINK_NAME);
    let sink = sink_element
        .as_ref()
        .and_then(|sink| sink.factory().map(|factory| factory.name().to_string()))
        .unwrap_or_else(|| "unknown".to_string());
    let sink_adapter = sink_element
        .as_ref()
        .and_then(|sink| element_numeric_property(sink, "adapter"))
        .unwrap_or_else(|| "default/unknown".to_string());
    let caps = caps.to_string();
    let memory = if caps.contains("memory:D3D12Memory") {
        "D3D12Memory"
    } else if caps.contains("memory:D3D11Memory") {
        "D3D11Memory"
    } else {
        "system/other memory"
    };

    crate::logging::append(format!(
        "receiver runtime decoder={decoder_factory} decoder-luid={decoder_luid} memory={memory} caps={caps} sink={sink} sink-adapter={sink_adapter}"
    ));
    true
}

fn element_numeric_property(element: &gst::Element, property: &str) -> Option<String> {
    element.find_property(property)?;
    let value = element.property_value(property);
    if let Ok(value) = value.get::<i64>() {
        return Some(value.to_string());
    }
    if let Ok(value) = value.get::<u64>() {
        return Some(value.to_string());
    }
    if let Ok(value) = value.get::<i32>() {
        return Some(value.to_string());
    }
    value.get::<u32>().ok().map(|value| value.to_string())
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
        format!(
            "d3d11convert ! video/x-raw(memory:D3D11Memory),format=NV12,framerate={fps}/1{size} \
             ! d3d11download ! video/x-raw,format=NV12,framerate={fps}/1{size}"
        )
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum EncoderFamily {
    Nvidia,
    Amf,
    MediaFoundation,
    QuickSync,
    X264,
}

impl EncoderFamily {
    /// Base element of the family. GStreamer registers one variant per GPU, named by inserting
    /// `deviceN` before the trailing role, so the base element is the device-0 variant.
    fn base_element(self) -> &'static str {
        match self {
            Self::Nvidia => "nvd3d11h264enc",
            Self::Amf => "amfh264enc",
            Self::MediaFoundation => "mfh264enc",
            Self::QuickSync => "qsvh264enc",
            Self::X264 => "x264enc",
        }
    }

    /// Whether an element factory belongs to this family, including its per-GPU variants such as
    /// `nvd3d11h264device1enc`, `qsvh264device1enc` and `amfh264device1enc`.
    fn owns_element(self, name: &str) -> bool {
        let prefix = match self {
            Self::Nvidia => "nvd3d11",
            Self::Amf => "amf",
            Self::MediaFoundation => "mf",
            Self::QuickSync => "qsv",
            // x264enc is a CPU encoder with no per-GPU variants.
            Self::X264 => return name == "x264enc",
        };
        name.starts_with(prefix) && name.contains("h264") && name.ends_with("enc")
    }

    /// Hardware families in the order they should be tried for one GPU vendor.
    fn preferred_for(vendor: crate::gpu::GpuVendor) -> &'static [Self] {
        match vendor {
            crate::gpu::GpuVendor::Nvidia => &[Self::Nvidia, Self::MediaFoundation],
            crate::gpu::GpuVendor::Amd => &[Self::Amf, Self::MediaFoundation],
            crate::gpu::GpuVendor::Intel => &[Self::QuickSync, Self::MediaFoundation],
            crate::gpu::GpuVendor::Other => &[
                Self::Nvidia,
                Self::QuickSync,
                Self::Amf,
                Self::MediaFoundation,
            ],
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SelectedEncoder {
    family: EncoderFamily,
    element: String,
    /// GPU the element is bound to, when it advertises one.
    adapter_luid: Option<i64>,
}

impl SelectedEncoder {
    fn supports_d3d11_input(&self) -> bool {
        if self.family == EncoderFamily::X264 {
            return false;
        }
        let d3d11_caps = gst::Caps::builder("video/x-raw")
            .features(["memory:D3D11Memory"])
            .field("format", "NV12")
            .build();
        gst::ElementFactory::find(&self.element)
            .is_some_and(|factory| factory.can_sink_any_caps(&d3d11_caps))
    }

    fn name(&self) -> &str {
        &self.element
    }

    fn chain(&self, bitrate: u32, fps: u32, nvidia_tuning: NvidiaTuning) -> String {
        let element = &self.element;
        let properties = match self.family {
            EncoderFamily::Nvidia => nvidia_encoder_properties(bitrate, fps, nvidia_tuning),
            EncoderFamily::Amf => vec![
                format!("bitrate={bitrate}"),
                format!("max-bitrate={bitrate}"),
                format!("gop-size={fps}"),
                "usage=ultra-low-latency".to_string(),
                "preset=speed".to_string(),
                "rate-control=cbr".to_string(),
                "aud=false".to_string(),
            ],
            EncoderFamily::MediaFoundation => vec![
                format!("bitrate={bitrate}"),
                format!("max-bitrate={bitrate}"),
                format!("gop-size={fps}"),
                "bframes=0".to_string(),
                "low-latency=true".to_string(),
                "rc-mode=cbr".to_string(),
                "quality-vs-speed=0".to_string(),
            ],
            EncoderFamily::QuickSync => vec![
                format!("bitrate={bitrate}"),
                format!("gop-size={fps}"),
                "b-frames=0".to_string(),
                "rc-lookahead=0".to_string(),
                "rate-control=cbr".to_string(),
            ],
            EncoderFamily::X264 => vec![
                format!("bitrate={bitrate}"),
                "speed-preset=ultrafast".to_string(),
                "tune=zerolatency".to_string(),
                format!("key-int-max={fps}"),
                "bframes=0".to_string(),
                "sliced-threads=true".to_string(),
                "byte-stream=true".to_string(),
            ],
        };
        let properties = supported_properties(element, &properties);

        format!(
            "{element} name={SENDER_VIDEO_ENCODER_NAME}{properties} \
             ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
        )
    }
}

/// Keeps only the tuning knobs the element actually exposes, prefixed with a space each.
///
/// Media Foundation registers per-MFT property sets, so `mfh264enc` on one machine has `bframes`
/// and on the next it does not; naming a property the element lacks fails the whole pipeline parse
/// and leaves the sender with no stream at all. A missing knob costs some latency tuning, which is
/// a far better outcome, so it is dropped with a log line instead.
fn supported_properties(element: &str, properties: &[String]) -> String {
    let mut kept = String::new();
    let mut dropped = Vec::new();
    // An element that cannot be created at all tells us nothing about its properties, so the
    // settings are kept as written and the pipeline reports the missing element itself.
    let instance = gst::ElementFactory::make(element).build().ok();
    for property in properties {
        let name = property.split('=').next().unwrap_or_default();
        let supported = instance
            .as_ref()
            .is_none_or(|instance| instance.find_property(name).is_some());
        if supported {
            kept.push(' ');
            kept.push_str(property);
        } else {
            dropped.push(name.to_string());
        }
    }
    if !dropped.is_empty() {
        crate::logging::append(format!(
            "encoder {element} does not expose {}; those settings were dropped",
            dropped.join(", ")
        ));
    }
    kept
}

fn nvidia_encoder_properties(bitrate: u32, fps: u32, tuning: NvidiaTuning) -> Vec<String> {
    let tuning = resolve_nvidia_tuning(tuning);
    let vbv_buffer_size = (bitrate / fps.max(1)).max(128);
    let mut properties = vec![
        format!("bitrate={bitrate}"),
        format!("max-bitrate={bitrate}"),
        format!("vbv-buffer-size={vbv_buffer_size}"),
        format!("gop-size={fps}"),
        "bframes=0".to_string(),
        "rc-lookahead=0".to_string(),
        "zerolatency=true".to_string(),
        "strict-gop=true".to_string(),
        "aud=false".to_string(),
        "repeat-sequence-header=true".to_string(),
    ];
    if tuning == NvidiaTuning::Rtx {
        properties.extend([
            "spatial-aq=true".to_string(),
            "temporal-aq=true".to_string(),
            "aq-strength=8".to_string(),
        ]);
    }
    properties
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

fn select_encoder(
    requested: Encoder,
    allow_software_encoder: bool,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Result<SelectedEncoder> {
    let family = match requested {
        Encoder::Auto => return first_available_encoder(allow_software_encoder, gpu),
        Encoder::Nvidia => EncoderFamily::Nvidia,
        Encoder::Amf => EncoderFamily::Amf,
        Encoder::MediaFoundation => EncoderFamily::MediaFoundation,
        Encoder::QuickSync => EncoderFamily::QuickSync,
        Encoder::X264 => EncoderFamily::X264,
    };

    encoder_on_gpu(family, gpu).ok_or_else(|| {
        anyhow!(
            "required GStreamer element not found: {}",
            family.base_element()
        )
    })
}

fn first_available_encoder(
    allow_software_encoder: bool,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Result<SelectedEncoder> {
    let families: &[EncoderFamily] = match gpu {
        Some(gpu) => EncoderFamily::preferred_for(gpu.vendor()),
        None => &[
            EncoderFamily::Nvidia,
            EncoderFamily::QuickSync,
            EncoderFamily::Amf,
            EncoderFamily::MediaFoundation,
        ],
    };

    if let Some(encoder) = families
        .iter()
        .find_map(|family| encoder_on_gpu(*family, gpu))
    {
        return Ok(encoder);
    }

    if allow_software_encoder {
        return encoder_on_gpu(EncoderFamily::X264, None)
            .ok_or_else(|| anyhow!("required GStreamer element not found: x264enc"));
    }

    Err(anyhow!(
        "no supported GPU H.264 encoder found; install GStreamer Bad plugins or pass --allow-software-encoder true to permit CPU x264"
    ))
}

/// Picks the family variant bound to the requested GPU, falling back to the family's default
/// variant when nothing advertises that GPU.
fn encoder_on_gpu(
    family: EncoderFamily,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Option<SelectedEncoder> {
    let elements = family_elements(gst::ElementFactoryType::VIDEO_ENCODER, |name| {
        family.owns_element(name)
    });
    if elements.is_empty() {
        return None;
    }

    if let Some(gpu) = gpu {
        if let Some(element) = elements
            .iter()
            .find(|element| element_adapter_luid(element) == Some(gpu.luid))
        {
            return Some(SelectedEncoder {
                family,
                element: element.clone(),
                adapter_luid: Some(gpu.luid),
            });
        }
        crate::logging::append(format!(
            "no {} variant is bound to GPU {}; using {}",
            family.base_element(),
            gpu.description,
            elements[0]
        ));
    }

    let element = elements[0].clone();
    Some(SelectedEncoder {
        adapter_luid: element_adapter_luid(&element),
        family,
        element,
    })
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ReceiverGpuProfile {
    Nvidia,
    Amd,
    Intel,
    Other,
}

impl ReceiverGpuProfile {
    fn label(self) -> &'static str {
        match self {
            Self::Nvidia => "nvidia",
            Self::Amd => "amd",
            Self::Intel => "intel",
            Self::Other => "other",
        }
    }

    fn route_label(self, memory: ReceiverMemory) -> &'static str {
        match (self, memory) {
            (Self::Intel, ReceiverMemory::D3d12) => "intel-modern-d3d12",
            _ => self.label(),
        }
    }
}

fn receiver_gpu_profile(gpu: Option<&crate::gpu::GpuAdapter>) -> ReceiverGpuProfile {
    match gpu.map(crate::gpu::GpuAdapter::vendor) {
        Some(crate::gpu::GpuVendor::Nvidia) => ReceiverGpuProfile::Nvidia,
        Some(crate::gpu::GpuVendor::Amd) => ReceiverGpuProfile::Amd,
        Some(crate::gpu::GpuVendor::Intel) => ReceiverGpuProfile::Intel,
        Some(crate::gpu::GpuVendor::Other) | None => ReceiverGpuProfile::Other,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum ReceiverMemory {
    D3d12,
    D3d11,
    Negotiated,
    System,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiverDecoder {
    factory: String,
    memory: ReceiverMemory,
    adapter_luid: Option<i64>,
    properties: String,
}

impl ReceiverDecoder {
    fn new(factory: impl Into<String>, memory: ReceiverMemory) -> Self {
        Self {
            factory: factory.into(),
            memory,
            adapter_luid: None,
            properties: String::new(),
        }
    }

    fn with_adapter_luid(mut self, luid: i64) -> Self {
        self.adapter_luid = Some(luid);
        self
    }

    fn with_system_memory(mut self) -> Self {
        self.memory = ReceiverMemory::System;
        self
    }

    fn with_property(mut self, name: &str, value: impl std::fmt::Display) -> Self {
        self.properties.push_str(&format!(" {name}={value}"));
        self
    }

    fn pipeline_element(&self) -> String {
        format!(
            "{} name={RECEIVER_VIDEO_DECODER_NAME}{}",
            self.factory, self.properties
        )
    }

    fn output_caps_for(&self, sink: &str) -> &'static str {
        match self.memory {
            ReceiverMemory::D3d12 if sink.starts_with("d3d12videosink") => {
                " ! video/x-raw(memory:D3D12Memory),format=NV12"
            }
            ReceiverMemory::D3d11 if sink.starts_with("d3d11videosink") => {
                " ! video/x-raw(memory:D3D11Memory),format=NV12"
            }
            ReceiverMemory::System => " ! video/x-raw",
            ReceiverMemory::D3d12 | ReceiverMemory::D3d11 | ReceiverMemory::Negotiated => "",
        }
    }

    fn memory_label_for(&self, sink: &str) -> &'static str {
        match self.memory {
            ReceiverMemory::D3d12 if sink.starts_with("d3d12videosink") => "D3D12Memory/NV12",
            ReceiverMemory::D3d11 if sink.starts_with("d3d11videosink") => "D3D11Memory/NV12",
            ReceiverMemory::System => "system-memory",
            ReceiverMemory::D3d12 | ReceiverMemory::D3d11 | ReceiverMemory::Negotiated => {
                "negotiated"
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReceiverVideoRoute {
    decoder: ReceiverDecoder,
    sink: String,
}

impl ReceiverVideoRoute {
    fn uses_d3d12(&self) -> bool {
        self.decoder.memory == ReceiverMemory::D3d12 && self.sink.starts_with("d3d12videosink")
    }
}

fn select_receiver_route(
    requested_decoder: Decoder,
    requested_sink: Sink,
    fullscreen: bool,
    gpu: Option<&crate::gpu::GpuAdapter>,
    allow_modern_intel_d3d12: bool,
) -> Result<ReceiverVideoRoute> {
    if allow_modern_intel_d3d12
        && should_try_modern_intel_d3d12(requested_decoder, requested_sink, gpu)
    {
        if let Some((decoder, sink)) =
            gpu.and_then(|gpu| d3d12_decoder_on_gpu(gpu).zip(d3d12_sink_on_gpu(fullscreen, gpu)))
        {
            crate::logging::append(format!(
                "receiver Intel GPU exposes a matching D3D12 H.264 route; using {} with D3D12 zero-copy",
                decoder.factory
            ));
            return Ok(ReceiverVideoRoute { decoder, sink });
        }
    }

    Ok(ReceiverVideoRoute {
        decoder: select_decoder(requested_decoder, gpu)?,
        sink: select_sink(requested_sink, fullscreen, gpu)?,
    })
}

fn should_try_modern_intel_d3d12(
    requested_decoder: Decoder,
    requested_sink: Sink,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> bool {
    requested_decoder == Decoder::Auto
        && requested_sink == Sink::Auto
        && gpu.is_some_and(|gpu| gpu.vendor() == crate::gpu::GpuVendor::Intel)
}

fn select_decoder(
    requested: Decoder,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Result<ReceiverDecoder> {
    match requested {
        Decoder::D3d11 => {
            if let Some(gpu) = gpu {
                if let Some(decoder) = d3d11_decoder_on_gpu(gpu) {
                    return Ok(decoder);
                }
                if selected_is_only_adapter(gpu, &crate::gpu::adapters()) {
                    crate::logging::append(format!(
                        "selected GPU {} is the only DXGI adapter; using the base D3D11 H.264 decoder",
                        gpu.description
                    ));
                    return require_element(
                        "d3d11h264dec",
                        ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11),
                    );
                }
                return Err(anyhow!(
                    "no D3D11 H.264 decoder is bound to selected GPU {}; refusing a cross-adapter decoder because decoder=d3d11 was explicitly requested",
                    gpu.description
                ));
            }
            require_element(
                "d3d11h264dec",
                ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11),
            )
        }
        Decoder::Avdec => require_element(
            "avdec_h264",
            ReceiverDecoder::new("avdec_h264", ReceiverMemory::Negotiated),
        ),
        Decoder::Software => software_decoder().ok_or_else(|| {
            anyhow!(
                "no software H.264 decoder is available; expected one of {}",
                SOFTWARE_H264_DECODERS.join(", ")
            )
        }),
        Decoder::Auto => {
            if let Some(decoder) = gpu.and_then(d3d11_decoder_on_gpu) {
                return Ok(decoder);
            }
            if let Some(gpu) = gpu.filter(|gpu| {
                selected_is_only_adapter(gpu, &crate::gpu::adapters())
                    && has_element("d3d11h264dec")
            }) {
                crate::logging::append(format!(
                    "receiver GPU {} is the only DXGI adapter; using the base D3D11 H.264 decoder",
                    gpu.description
                ));
                return Ok(ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11));
            }
            if gpu.is_none() && has_element("d3d11h264dec") {
                crate::logging::append(
                    "receiver automatic GPU is unavailable; using the primary D3D11 H.264 decoder",
                );
                return Ok(ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11));
            }

            let profile = receiver_gpu_profile(gpu);
            let fallback = match profile {
                ReceiverGpuProfile::Nvidia => gpu.and_then(nvidia_decoder_on_gpu),
                ReceiverGpuProfile::Intel => gpu.and_then(qsv_decoder_on_gpu),
                ReceiverGpuProfile::Amd | ReceiverGpuProfile::Other => None,
            };
            if let Some(decoder) = fallback {
                crate::logging::append(format!(
                    "receiver {} GPU has no matching D3D11 H.264 decoder; using {}",
                    profile.label(),
                    decoder.factory
                ));
                return Ok(decoder);
            }

            crate::logging::append(format!(
                "receiver {} GPU has no matching hardware H.264 decoder; using decodebin with system-memory output to avoid cross-adapter textures",
                profile.label()
            ));
            require_element(
                "decodebin",
                ReceiverDecoder::new("decodebin", ReceiverMemory::Negotiated).with_system_memory(),
            )
        }
    }
}

/// Ordered by preference. The MSI bundles openh264 (BSD) and deliberately omits the
/// GPL/LGPL-encumbered libav and x264 plugins, so `openh264dec` is the only software decoder that
/// exists on an installed build; `avdec_h264` stays as a second choice for developer machines
/// running a full GStreamer install.
const SOFTWARE_H264_DECODERS: [&str; 2] = ["openh264dec", "avdec_h264"];

fn software_decoder() -> Option<ReceiverDecoder> {
    SOFTWARE_H264_DECODERS
        .into_iter()
        .find(|factory| has_element(factory))
        .map(|factory| ReceiverDecoder::new(factory, ReceiverMemory::System))
}

fn nvidia_decoder_on_gpu(gpu: &crate::gpu::GpuAdapter) -> Option<ReceiverDecoder> {
    family_decoder_on_gpu(
        gpu,
        |name| name.starts_with("nvh264") && name.ends_with("dec"),
        ReceiverMemory::D3d11,
    )
    .or_else(|| {
        if selected_is_only_vendor_adapter(gpu, &crate::gpu::adapters()) {
            nvidia_decoder("nvh264dec")
        } else {
            None
        }
    })
}

fn qsv_decoder_on_gpu(gpu: &crate::gpu::GpuAdapter) -> Option<ReceiverDecoder> {
    family_decoder_on_gpu(
        gpu,
        |name| name.starts_with("qsvh264") && name.ends_with("dec"),
        ReceiverMemory::D3d11,
    )
    .or_else(|| {
        if selected_is_only_vendor_adapter(gpu, &crate::gpu::adapters()) {
            decoder_with_preferred_memory("qsvh264dec", ReceiverMemory::D3d11)
        } else {
            None
        }
    })
}

fn nvidia_decoder(factory: &str) -> Option<ReceiverDecoder> {
    if !has_element(factory) {
        return None;
    }
    let memory = if factory_supports_raw_memory_src(factory, ReceiverMemory::D3d11) {
        ReceiverMemory::D3d11
    } else {
        ReceiverMemory::Negotiated
    };
    let mut decoder = ReceiverDecoder::new(factory, memory);
    if element_has_property(factory, "max-display-delay") {
        decoder = decoder.with_property("max-display-delay", 0);
    }
    Some(decoder)
}

fn decoder_with_preferred_memory(
    factory: &str,
    preferred_memory: ReceiverMemory,
) -> Option<ReceiverDecoder> {
    has_element(factory).then(|| {
        let memory = if factory_supports_raw_memory_src(factory, preferred_memory) {
            preferred_memory
        } else {
            ReceiverMemory::Negotiated
        };
        ReceiverDecoder::new(factory, memory)
    })
}

fn family_decoder_on_gpu(
    gpu: &crate::gpu::GpuAdapter,
    predicate: impl Fn(&str) -> bool,
    preferred_memory: ReceiverMemory,
) -> Option<ReceiverDecoder> {
    family_elements(gst::ElementFactoryType::DECODER, predicate)
        .into_iter()
        .find(|element| element_adapter_luid(element) == Some(gpu.luid))
        .and_then(|factory| {
            if factory.starts_with("nvh264") {
                nvidia_decoder(&factory)
            } else {
                decoder_with_preferred_memory(&factory, preferred_memory)
            }
        })
        .map(|decoder| decoder.with_adapter_luid(gpu.luid))
}

fn selected_is_only_adapter(
    selected: &crate::gpu::GpuAdapter,
    adapters: &[crate::gpu::GpuAdapter],
) -> bool {
    adapters.len() == 1 && adapters[0].luid == selected.luid
}

fn selected_is_only_vendor_adapter(
    selected: &crate::gpu::GpuAdapter,
    adapters: &[crate::gpu::GpuAdapter],
) -> bool {
    let matches: Vec<&crate::gpu::GpuAdapter> = adapters
        .iter()
        .filter(|adapter| adapter.vendor() == selected.vendor())
        .collect();
    matches.len() == 1 && matches[0].luid == selected.luid
}

fn d3d11_decoder_on_gpu(gpu: &crate::gpu::GpuAdapter) -> Option<ReceiverDecoder> {
    let decoder = family_decoder_on_gpu(
        gpu,
        |name| name.starts_with("d3d11") && name.contains("h264") && name.ends_with("dec"),
        ReceiverMemory::D3d11,
    );

    if decoder.is_none() {
        crate::logging::append(format!(
            "no d3d11 H.264 decoder is bound to GPU {}",
            gpu.description
        ));
    }
    decoder
}

fn d3d12_decoder_on_gpu(gpu: &crate::gpu::GpuAdapter) -> Option<ReceiverDecoder> {
    let decoder = family_decoder_on_gpu(
        gpu,
        |name| name.starts_with("d3d12") && name.contains("h264") && name.ends_with("dec"),
        ReceiverMemory::D3d12,
    )
    .filter(|decoder| decoder.memory == ReceiverMemory::D3d12);

    if decoder.is_none() {
        crate::logging::append(format!(
            "no D3D12Memory H.264 decoder is bound to GPU {}",
            gpu.description
        ));
    }
    decoder
}

fn d3d12_sink_on_gpu(fullscreen: bool, gpu: &crate::gpu::GpuAdapter) -> Option<String> {
    if !factory_supports_raw_memory_sink("d3d12videosink", ReceiverMemory::D3d12) {
        return None;
    }

    let fullscreen = if fullscreen { " fullscreen=true" } else { "" };
    Some(format!(
        "d3d12videosink name={RECEIVER_VIDEO_SINK_NAME} adapter={} sync=false async=false qos=true enable-last-sample=false{fullscreen}",
        gpu.index
    ))
}

fn select_sink(
    requested: Sink,
    fullscreen: bool,
    gpu: Option<&crate::gpu::GpuAdapter>,
) -> Result<String> {
    let adapter = match gpu {
        Some(gpu) => format!(" adapter={}", gpu.index),
        None => String::new(),
    };
    let d3d11_sink = if fullscreen {
        format!(
            "d3d11videosink name={RECEIVER_VIDEO_SINK_NAME}{adapter} sync=false async=false qos=true enable-last-sample=false fullscreen-toggle-mode=property fullscreen=true"
        )
    } else {
        format!("d3d11videosink name={RECEIVER_VIDEO_SINK_NAME}{adapter} sync=false async=false qos=true enable-last-sample=false")
    };

    match requested {
        Sink::Auto => {
            if has_element("d3d11videosink") {
                Ok(d3d11_sink)
            } else {
                Ok(format!(
                    "autovideosink name={RECEIVER_VIDEO_SINK_NAME} sync=false async=false"
                ))
            }
        }
        Sink::D3d11 => require_element("d3d11videosink", d3d11_sink),
        Sink::AutoVideo => Ok(format!(
            "autovideosink name={RECEIVER_VIDEO_SINK_NAME} sync=false async=false"
        )),
    }
}

/// Registered element factory names matching a predicate, shortest name first so the family's
/// base (device-0) element wins ties.
fn family_elements(
    factory_type: gst::ElementFactoryType,
    predicate: impl Fn(&str) -> bool,
) -> Vec<String> {
    // Rank is an autoplugging hint, and hardware encoders are commonly registered below MARGINAL
    // so decodebin does not pick them on its own: NVENC's D3D11 elements are rank NONE. The
    // predicate already names the family we want, so every rank has to be enumerated or a present
    // encoder looks missing and the sender silently drops to a weaker family.
    let mut names: Vec<String> =
        gst::ElementFactory::factories_with_type(factory_type, gst::Rank::NONE)
            .iter()
            .map(|factory| factory.name().to_string())
            .filter(|name| predicate(name))
            .collect();
    names.sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    names
}

fn raw_memory_caps(memory: ReceiverMemory) -> Option<gst::Caps> {
    let feature = match memory {
        ReceiverMemory::D3d12 => "memory:D3D12Memory",
        ReceiverMemory::D3d11 => "memory:D3D11Memory",
        ReceiverMemory::Negotiated | ReceiverMemory::System => return None,
    };
    Some(
        gst::Caps::builder("video/x-raw")
            .features([feature])
            .field("format", "NV12")
            .build(),
    )
}

fn factory_supports_raw_memory_src(factory: &str, memory: ReceiverMemory) -> bool {
    let Some(caps) = raw_memory_caps(memory) else {
        return false;
    };
    gst::ElementFactory::find(factory).is_some_and(|factory| factory.can_src_any_caps(&caps))
}

fn factory_supports_raw_memory_sink(factory: &str, memory: ReceiverMemory) -> bool {
    let Some(caps) = raw_memory_caps(memory) else {
        return false;
    };
    gst::ElementFactory::find(factory).is_some_and(|factory| factory.can_sink_any_caps(&caps))
}

/// The GPU an element is bound to, read from the `adapter-luid` property the d3d11, nvcodec, qsv
/// and amf elements expose. `None` means the element does not advertise a GPU.
fn element_adapter_luid(name: &str) -> Option<i64> {
    use gst::glib;

    let element = gst::ElementFactory::make(name).build().ok()?;
    let property = element.find_property("adapter-luid")?;
    if property.value_type() != glib::Type::I64 {
        return None;
    }
    Some(element.property::<i64>("adapter-luid"))
}

fn element_has_property(name: &str, property: &str) -> bool {
    gst::ElementFactory::make(name)
        .build()
        .ok()
        .is_some_and(|element| element.find_property(property).is_some())
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
    for (role, name) in probed_elements() {
        crate::console::line(format!(
            "{role:12} {name:24} {}",
            if has_element(name) { "yes" } else { "no" }
        ));
    }
}

/// What `probe` reports on, in the order a reader scans it.
fn probed_elements() -> Vec<(&'static str, &'static str)> {
    let mut elements = vec![
        ("capture", "d3d11screencapturesrc"),
        ("gpu encode", "nvd3d11h264enc"),
        ("amf encode", "amfh264enc"),
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
        ("d3d12 decode", "d3d12h264dec"),
        ("gpu decode", "d3d11h264dec"),
        ("qsv decode", "qsvh264dec"),
    ];
    // Every software decoder the receiver would actually try, in the order it tries them. Naming one
    // by hand reported "cpu decode: no" on a machine whose bundled openh264 decoder was present and
    // working, which reads as "this build cannot decode without a GPU" when the opposite is true.
    elements.extend(SOFTWARE_H264_DECODERS.map(|name| ("cpu decode", name)));
    elements.push(("d3d12 sink", "d3d12videosink"));
    elements.push(("gpu sink", "d3d11videosink"));
    elements
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_reports_on_every_decoder_the_receiver_can_fall_back_to() {
        let probed = probed_elements();

        for decoder in SOFTWARE_H264_DECODERS {
            assert!(
                probed.contains(&("cpu decode", decoder)),
                "probe never mentions {decoder}, so a build that ships it looks like it cannot decode on the CPU"
            );
        }
    }

    fn h264_nal_types_from_rtp(packet: &[u8]) -> Vec<u8> {
        if packet.len() < 12 || packet[0] >> 6 != 2 {
            return Vec::new();
        }

        let mut payload_offset = 12 + usize::from(packet[0] & 0x0f) * 4;
        if packet[0] & 0x10 != 0 {
            if payload_offset + 4 > packet.len() {
                return Vec::new();
            }
            let extension_words =
                u16::from_be_bytes([packet[payload_offset + 2], packet[payload_offset + 3]]);
            payload_offset += 4 + usize::from(extension_words) * 4;
        }
        let padding = if packet[0] & 0x20 != 0 {
            usize::from(*packet.last().unwrap_or(&0))
        } else {
            0
        };
        if payload_offset >= packet.len() || padding > packet.len() - payload_offset {
            return Vec::new();
        }

        let payload = &packet[payload_offset..packet.len() - padding];
        let packet_type = payload[0] & 0x1f;
        match packet_type {
            1..=23 => vec![packet_type],
            24 => {
                let mut types = Vec::new();
                let mut offset = 1;
                while offset + 2 <= payload.len() {
                    let nal_size =
                        usize::from(u16::from_be_bytes([payload[offset], payload[offset + 1]]));
                    offset += 2;
                    if nal_size == 0 || offset + nal_size > payload.len() {
                        break;
                    }
                    types.push(payload[offset] & 0x1f);
                    offset += nal_size;
                }
                types
            }
            28 if payload.len() >= 2 && payload[1] & 0x80 != 0 => vec![payload[1] & 0x1f],
            _ => Vec::new(),
        }
    }

    fn virtual_monitor(handle: Option<u64>, bundled: bool) -> crate::monitors::DisplayMonitor {
        crate::monitors::DisplayMonitor {
            capture_index: Some(2),
            monitor_handle: handle,
            adapter_name: r"\\.\DISPLAY24".to_string(),
            adapter_description: "Virtual Display Driver".to_string(),
            monitor_name: None,
            monitor_description: Some("Generic Monitor (VDD by MTT)".to_string()),
            device_id: r"MONITOR\MTT1337\test".to_string(),
            attached: true,
            primary: false,
            mirroring: false,
            virtual_candidate: true,
            bundled_virtual_display: bundled,
        }
    }

    #[test]
    fn stable_monitor_handle_wins_over_ambiguous_index() {
        let source = capture_source_for_target(
            CaptureApi::Dxgi,
            "true",
            &virtual_monitor(Some(31_199_895), true),
        )
        .unwrap();

        assert!(source.contains("capture-api=dxgi"));
        assert!(source.contains("monitor-handle=31199895"));
        assert!(!source.contains("monitor-index="));
    }

    #[test]
    fn bundled_vdd_without_handle_does_not_fall_back_to_index() {
        let error =
            capture_source_for_target(CaptureApi::Dxgi, "true", &virtual_monitor(None, true))
                .unwrap_err();

        assert!(error
            .to_string()
            .contains("refusing ambiguous monitor-index"));
    }

    #[test]
    fn d3d11_caps_keep_frames_on_gpu_without_download() {
        let caps = video_caps(30, Some(1920), Some(1080), true);
        assert!(caps.contains("memory:D3D11Memory"));
        assert!(!caps.contains("d3d11download"));
    }

    #[test]
    fn system_memory_caps_convert_to_nv12_before_download() {
        let caps = video_caps(30, Some(1920), Some(1080), false);
        let gpu_nv12 = caps
            .find("video/x-raw(memory:D3D11Memory),format=NV12")
            .expect("GPU NV12 conversion");
        let download = caps.find("d3d11download").expect("D3D11 download");
        let system_nv12 = caps
            .rfind("video/x-raw,format=NV12")
            .expect("system-memory NV12 caps");

        assert!(gpu_nv12 < download);
        assert!(download < system_nv12);
        assert!(!caps.contains("videoconvert"));
    }

    #[test]
    fn a_stream_report_separates_frames_that_never_arrived_from_frames_that_were_dropped() {
        let line = stream_report_line(
            Duration::from_secs(5),
            35,
            Some(70),
            Some(JitterCounts {
                pushed: 3489,
                lost: 12,
                late: 4,
                duplicates: 0,
            }),
        );

        assert_eq!(
            line,
            "receiver stream 5.0s: decoded=70 (14.0 fps) displayed=35 (7.0 fps) \
             rtp pushed=3489 lost=12 late=4 duplicates=0"
        );
    }

    #[test]
    fn a_stream_report_still_names_a_rate_when_the_decoder_cannot_be_counted() {
        let line = stream_report_line(Duration::from_millis(5_000), 150, None, None);

        assert_eq!(line, "receiver stream 5.0s: displayed=150 (30.0 fps)");
    }

    /// The jitter buffer reports running totals, so a report window has to subtract the last one.
    /// Reporting the totals instead made every window look worse than the one before it.
    #[test]
    fn jitter_counts_report_the_window_and_not_the_session() {
        let earlier = JitterCounts {
            pushed: 1_000,
            lost: 5,
            late: 2,
            duplicates: 1,
        };
        let now = JitterCounts {
            pushed: 4_500,
            lost: 5,
            late: 9,
            duplicates: 1,
        };

        assert_eq!(
            now.since(earlier),
            JitterCounts {
                pushed: 3_500,
                lost: 0,
                late: 7,
                duplicates: 0,
            }
        );
    }

    /// A restarted jitter buffer resets its totals, and a window that spans the restart must not
    /// wrap into billions of packets.
    #[test]
    fn jitter_counts_that_went_backwards_report_nothing_rather_than_wrapping() {
        let after_reset = JitterCounts::default().since(JitterCounts {
            pushed: 4_500,
            lost: 5,
            late: 9,
            duplicates: 1,
        });

        assert_eq!(after_reset, JitterCounts::default());
    }

    #[test]
    fn receiver_gpu_profiles_follow_vendor_ids() {
        let adapter = |vendor_id| crate::gpu::GpuAdapter {
            index: 0,
            luid: 1,
            vendor_id,
            device_id: 0,
            description: "test".to_string(),
        };

        assert_eq!(
            receiver_gpu_profile(Some(&adapter(0x10DE))),
            ReceiverGpuProfile::Nvidia
        );
        assert_eq!(
            receiver_gpu_profile(Some(&adapter(0x1002))),
            ReceiverGpuProfile::Amd
        );
        assert_eq!(
            receiver_gpu_profile(Some(&adapter(0x8086))),
            ReceiverGpuProfile::Intel
        );
        assert_eq!(receiver_gpu_profile(None), ReceiverGpuProfile::Other);
    }

    #[test]
    fn d3d11_receiver_decoder_requires_gpu_memory_caps() {
        let decoder = ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11);

        assert!(decoder
            .pipeline_element()
            .contains("name=receiver_video_decoder"));
        assert_eq!(
            decoder.output_caps_for("d3d11videosink"),
            " ! video/x-raw(memory:D3D11Memory),format=NV12"
        );
        assert_eq!(
            decoder.memory_label_for("d3d11videosink"),
            "D3D11Memory/NV12"
        );
        assert_eq!(decoder.output_caps_for("autovideosink"), "");
    }

    #[test]
    fn explicit_auto_video_sink_keeps_d3d11_memory_negotiated() {
        let decoder = ReceiverDecoder::new("d3d11h264dec", ReceiverMemory::D3d11);

        assert_eq!(decoder.output_caps_for("autovideosink"), "");
        assert_eq!(decoder.memory_label_for("autovideosink"), "negotiated");
    }

    #[test]
    fn software_or_vendor_fallback_decoder_leaves_memory_negotiated() {
        let decoder = ReceiverDecoder::new("decodebin", ReceiverMemory::Negotiated);

        assert_eq!(decoder.output_caps_for("d3d11videosink"), "");
        assert_eq!(decoder.memory_label_for("d3d11videosink"), "negotiated");
    }

    #[test]
    fn cross_adapter_fallback_forces_system_memory() {
        let decoder =
            ReceiverDecoder::new("decodebin", ReceiverMemory::Negotiated).with_system_memory();

        assert_eq!(decoder.output_caps_for("d3d11videosink"), " ! video/x-raw");
        assert_eq!(decoder.memory_label_for("d3d11videosink"), "system-memory");
    }

    #[test]
    fn d3d12_receiver_route_pins_d3d12_memory() {
        let decoder = ReceiverDecoder::new("d3d12h264dec", ReceiverMemory::D3d12);

        assert_eq!(
            decoder.output_caps_for("d3d12videosink"),
            " ! video/x-raw(memory:D3D12Memory),format=NV12"
        );
        assert_eq!(
            decoder.memory_label_for("d3d12videosink"),
            "D3D12Memory/NV12"
        );
        assert_eq!(decoder.output_caps_for("d3d11videosink"), "");
    }

    #[test]
    fn modern_intel_d3d12_route_requires_both_auto_settings() {
        let intel = crate::gpu::GpuAdapter {
            index: 0,
            luid: 1,
            vendor_id: 0x8086,
            device_id: 0xB080,
            description: "Intel Arc B390".to_string(),
        };
        let nvidia = crate::gpu::GpuAdapter {
            vendor_id: 0x10DE,
            ..intel.clone()
        };

        assert!(should_try_modern_intel_d3d12(
            Decoder::Auto,
            Sink::Auto,
            Some(&intel)
        ));
        assert!(!should_try_modern_intel_d3d12(
            Decoder::D3d11,
            Sink::Auto,
            Some(&intel)
        ));
        assert!(!should_try_modern_intel_d3d12(
            Decoder::Auto,
            Sink::D3d11,
            Some(&intel)
        ));
        assert!(!should_try_modern_intel_d3d12(
            Decoder::Auto,
            Sink::Auto,
            Some(&nvidia)
        ));
    }

    #[test]
    fn receiver_plan_retries_compatible_route_after_primary_error() {
        gst::init().expect("GStreamer initialization");
        if ["videotestsrc", "fakesink"]
            .iter()
            .any(|name| gst::ElementFactory::find(name).is_none())
        {
            eprintln!("skipping receiver fallback integration test: test elements missing");
            return;
        }

        // Both hardware routes fail the way an out-of-caps stream does, so only the last route in
        // the chain can keep the receiver showing a picture.
        let plan = ReceiverPipelinePlan {
            primary: "screen_mirror_missing_d3d12_element ! fakesink".to_string(),
            fallbacks: vec![
                ReceiverFallbackRoute {
                    label: "D3D11/QSV",
                    description: "screen_mirror_missing_d3d11_element ! fakesink".to_string(),
                },
                ReceiverFallbackRoute {
                    label: "software",
                    description: "videotestsrc num-buffers=1 ! fakesink sync=false".to_string(),
                },
            ],
            decode_limits: None,
        };

        run_receiver_pipeline_plan(plan).expect("compatible receiver fallback should reach EOS");
    }

    #[test]
    fn receiver_plan_ends_with_a_software_route() {
        gst::init().expect("GStreamer initialization");
        let Some(software) = software_decoder() else {
            eprintln!("skipping software fallback test: no software H.264 decoder installed");
            return;
        };
        let args = crate::config::AppConfig::default().recv_args();
        let Ok(plan) = build_receiver_video_plan(&args) else {
            eprintln!("skipping software fallback test: no receiver route on this machine");
            return;
        };

        let last = plan
            .fallbacks
            .last()
            .expect("a software route after the hardware routes");
        assert_eq!(last.label, "software");
        assert!(last.description.contains(&software.factory));
        // Software decode lands in system memory, so the caps filter must not demand GPU memory.
        assert!(last.description.contains("! video/x-raw !"));
    }

    #[test]
    fn explicit_software_decoder_is_not_repeated_as_a_fallback() {
        gst::init().expect("GStreamer initialization");
        if software_decoder().is_none() {
            eprintln!("skipping software fallback test: no software H.264 decoder installed");
            return;
        }
        let mut args = crate::config::AppConfig::default().recv_args();
        args.decoder = Decoder::Software;
        let Ok(plan) = build_receiver_video_plan(&args) else {
            eprintln!("skipping software fallback test: no receiver route on this machine");
            return;
        };

        assert!(plan.fallbacks.iter().all(|route| route.label != "software"));
    }

    #[test]
    fn appending_audio_extends_every_receiver_route() {
        let mut plan = ReceiverPipelinePlan {
            primary: "primary".to_string(),
            fallbacks: vec![
                ReceiverFallbackRoute {
                    label: "D3D11/QSV",
                    description: "d3d11".to_string(),
                },
                ReceiverFallbackRoute {
                    label: "software",
                    description: "software".to_string(),
                },
            ],
            decode_limits: None,
        };

        plan.append_pipeline("audio");

        assert_eq!(plan.primary(), "primary audio");
        assert_eq!(plan.fallbacks[0].description, "d3d11 audio");
        assert_eq!(plan.fallbacks[1].description, "software audio");
    }

    #[test]
    fn vendor_base_decoder_is_safe_only_with_one_matching_vendor_adapter() {
        let selected = crate::gpu::GpuAdapter {
            index: 1,
            luid: 2,
            vendor_id: 0x10DE,
            device_id: 0,
            description: "selected".to_string(),
        };
        let intel = crate::gpu::GpuAdapter {
            index: 0,
            luid: 1,
            vendor_id: 0x8086,
            device_id: 0,
            description: "integrated".to_string(),
        };
        let second_nvidia = crate::gpu::GpuAdapter {
            index: 2,
            luid: 3,
            vendor_id: 0x10DE,
            device_id: 0,
            description: "second".to_string(),
        };

        assert!(selected_is_only_vendor_adapter(
            &selected,
            &[intel.clone(), selected.clone()]
        ));
        assert!(!selected_is_only_vendor_adapter(
            &selected,
            &[intel, selected.clone(), second_nvidia]
        ));
        assert!(!selected_is_only_adapter(
            &selected,
            &[selected.clone(), selected.clone()]
        ));
    }

    #[test]
    fn explicit_capture_index_resolves_its_monitor() {
        let mut primary = virtual_monitor(Some(10), false);
        primary.capture_index = Some(0);
        primary.primary = true;
        primary.adapter_name = r"\\.\DISPLAY1".to_string();

        let mut selected = virtual_monitor(Some(20), false);
        selected.capture_index = Some(2);
        selected.primary = false;
        selected.adapter_name = r"\\.\DISPLAY3".to_string();

        let monitors = [primary, selected];
        let monitor = capture_monitor_for_index(&monitors, 2).unwrap();

        assert_eq!(monitor.adapter_name, r"\\.\DISPLAY3");
    }

    #[test]
    fn encoder_speed_profiles_are_used_for_live_streaming() {
        gst::init().expect("GStreamer initialization");
        let amf = SelectedEncoder {
            family: EncoderFamily::Amf,
            element: "amfh264enc".to_string(),
            adapter_luid: None,
        }
        .chain(8_000, 30, NvidiaTuning::Auto);
        let media_foundation = SelectedEncoder {
            family: EncoderFamily::MediaFoundation,
            element: "mfh264enc".to_string(),
            adapter_luid: None,
        }
        .chain(8_000, 30, NvidiaTuning::Auto);

        assert!(amf.contains("usage=ultra-low-latency"));
        assert!(amf.contains("preset=speed"));
        assert!(media_foundation.contains("quality-vs-speed=0"));
        assert!(!media_foundation.contains("quality-vs-speed=100"));
    }

    #[test]
    fn every_encoder_chain_exposes_the_force_key_unit_target() {
        gst::init().expect("GStreamer initialization");
        for family in [
            EncoderFamily::Nvidia,
            EncoderFamily::Amf,
            EncoderFamily::MediaFoundation,
            EncoderFamily::QuickSync,
            EncoderFamily::X264,
        ] {
            let chain = SelectedEncoder {
                family,
                element: family.base_element().to_string(),
                adapter_luid: None,
            }
            .chain(8_000, 30, NvidiaTuning::Auto);

            assert!(
                chain.contains(&format!("name={SENDER_VIDEO_ENCODER_NAME}")),
                "{family:?} chain did not name its encoder: {chain}"
            );
        }
    }

    #[test]
    fn every_installed_encoder_chain_parses_on_this_machine() {
        gst::init().expect("GStreamer initialization");
        for family in [
            EncoderFamily::Nvidia,
            EncoderFamily::Amf,
            EncoderFamily::MediaFoundation,
            EncoderFamily::QuickSync,
            EncoderFamily::X264,
        ] {
            let Some(encoder) = encoder_on_gpu(family, None) else {
                continue;
            };
            let chain = encoder.chain(8_000, 30, NvidiaTuning::Auto);
            // Only the element and its properties: the caps filter that follows uses commas, which
            // `bin.( ... )` reads as its own property separators.
            let element = chain.split(" ! ").next().expect("encoder element");

            if let Err(error) = gst::parse::bin_from_description(element, true) {
                panic!("{family:?} encoder does not accept its settings: {element}\n{error}");
            }
        }
    }

    #[test]
    fn force_key_unit_schedule_requests_immediately_and_once_per_second() {
        let start = Instant::now();
        let mut schedule = KeyUnitSchedule::new(start);

        assert_eq!(schedule.take_due(start), Some(0));
        assert_eq!(
            schedule.take_due(start + FORCE_KEY_UNIT_INTERVAL - Duration::from_millis(1)),
            None
        );
        assert_eq!(schedule.take_due(start + FORCE_KEY_UNIT_INTERVAL), Some(1));
    }

    #[test]
    fn force_key_unit_event_requests_immediate_idr_with_headers() {
        gst::init().expect("GStreamer initialization");
        let event = upstream_force_key_unit_event(7);
        let structure = event.structure().expect("force-key-unit structure");

        assert_eq!(event.type_(), gst::EventType::CustomUpstream);
        assert_eq!(structure.name(), "GstForceKeyUnit");
        assert_eq!(
            structure
                .get::<Option<gst::ClockTime>>("running-time")
                .expect("running-time"),
            None
        );
        assert!(structure.get::<bool>("all-headers").expect("all-headers"));
        assert_eq!(structure.get::<u32>("count").expect("count"), 7);
    }

    #[test]
    fn force_key_unit_event_reaches_an_encoder_and_produces_a_new_keyframe() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        gst::init().expect("GStreamer initialization");
        if [
            "videotestsrc",
            "x264enc",
            "h264parse",
            "rtph264pay",
            "fakesink",
        ]
        .iter()
        .any(|name| gst::ElementFactory::find(name).is_none())
        {
            eprintln!("skipping force-key-unit integration test: GStreamer test elements missing");
            return;
        }

        let pipeline = gst::parse::launch(
            "videotestsrc is-live=true \
             ! video/x-raw,width=160,height=90,framerate=30/1 \
             ! x264enc name=sender_video_encoder tune=zerolatency speed-preset=ultrafast \
               key-int-max=300 bframes=0 byte-stream=true \
             ! h264parse config-interval=-1 \
             ! rtph264pay name=sender_rtp_pay config-interval=-1 aggregate-mode=zero-latency \
             ! fakesink sync=false",
        )
        .expect("force-key-unit test pipeline")
        .downcast::<gst::Pipeline>()
        .expect("GStreamer pipeline");

        let frames = Arc::new(AtomicUsize::new(0));
        let keyframes = Arc::new(AtomicUsize::new(0));
        let sps = Arc::new(AtomicUsize::new(0));
        let pps = Arc::new(AtomicUsize::new(0));
        let idr = Arc::new(AtomicUsize::new(0));
        let frames_for_probe = Arc::clone(&frames);
        let keyframes_for_probe = Arc::clone(&keyframes);
        pipeline
            .by_name(SENDER_VIDEO_ENCODER_NAME)
            .expect("named encoder")
            .static_pad("src")
            .expect("encoder src pad")
            .add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                if let Some(buffer) = info.buffer() {
                    frames_for_probe.fetch_add(1, Ordering::Relaxed);
                    if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                        keyframes_for_probe.fetch_add(1, Ordering::Relaxed);
                    }
                }
                gst::PadProbeReturn::Ok
            });
        let sps_for_probe = Arc::clone(&sps);
        let pps_for_probe = Arc::clone(&pps);
        let idr_for_probe = Arc::clone(&idr);
        pipeline
            .by_name(SENDER_RTP_PAY_NAME)
            .expect("named RTP payloader")
            .static_pad("src")
            .expect("payloader src pad")
            .add_probe(
                gst::PadProbeType::BUFFER | gst::PadProbeType::BUFFER_LIST,
                move |_pad, info| {
                    let observe_packet = |buffer: &gst::BufferRef| {
                        if let Ok(map) = buffer.map_readable() {
                            for nal_type in h264_nal_types_from_rtp(map.as_slice()) {
                                match nal_type {
                                    5 => {
                                        idr_for_probe.fetch_add(1, Ordering::Relaxed);
                                    }
                                    7 => {
                                        sps_for_probe.fetch_add(1, Ordering::Relaxed);
                                    }
                                    8 => {
                                        pps_for_probe.fetch_add(1, Ordering::Relaxed);
                                    }
                                    _ => {}
                                }
                            }
                        }
                    };

                    if let Some(buffer) = info.buffer() {
                        observe_packet(buffer.as_ref());
                    }
                    if let Some(buffer_list) = info.buffer_list() {
                        for buffer in buffer_list.iter() {
                            observe_packet(buffer);
                        }
                    }
                    gst::PadProbeReturn::Ok
                },
            );

        pipeline
            .set_state(gst::State::Playing)
            .expect("start force-key-unit test pipeline");
        let wait_until = |predicate: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if predicate() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            predicate()
        };
        let started = wait_until(&|| {
            frames.load(Ordering::Relaxed) >= 5
                && keyframes.load(Ordering::Relaxed) >= 1
                && sps.load(Ordering::Relaxed) >= 1
                && pps.load(Ordering::Relaxed) >= 1
                && idr.load(Ordering::Relaxed) >= 1
        });
        let headers_before = (
            sps.load(Ordering::Relaxed),
            pps.load(Ordering::Relaxed),
            idr.load(Ordering::Relaxed),
        );

        let mut requester = SenderKeyUnitRequester::attach(&pipeline).expect("named RTP payloader");
        let requested = requester.request_if_due(Instant::now());
        let accepted = requester.last_accepted;
        let forced = wait_until(&|| {
            keyframes.load(Ordering::Relaxed) >= 2
                && sps.load(Ordering::Relaxed) > headers_before.0
                && pps.load(Ordering::Relaxed) > headers_before.1
                && idr.load(Ordering::Relaxed) > headers_before.2
        });

        pipeline
            .set_state(gst::State::Null)
            .expect("stop force-key-unit test pipeline");
        assert!(
            started,
            "test pipeline did not produce its initial keyframe: frames={} keyframes={} sps={} pps={} idr={}",
            frames.load(Ordering::Relaxed),
            keyframes.load(Ordering::Relaxed),
            sps.load(Ordering::Relaxed),
            pps.load(Ordering::Relaxed),
            idr.load(Ordering::Relaxed)
        );
        assert!(
            requested,
            "initial force-key-unit request was not scheduled"
        );
        assert_eq!(
            accepted,
            Some(true),
            "encoder rejected force-key-unit event"
        );
        assert!(
            forced,
            "forced keyframe did not include SPS/PPS/IDR within 3s: before={headers_before:?} after=({},{},{})",
            sps.load(Ordering::Relaxed),
            pps.load(Ordering::Relaxed),
            idr.load(Ordering::Relaxed)
        );
    }

    #[test]
    fn a_hardware_decoder_reports_the_frame_size_it_accepts() {
        gst::init().expect("GStreamer initialization");

        if let Some((width, height)) = decoder_frame_limits("d3d11h264dec") {
            // Every DXVA H.264 decoder takes at least 1080p. What matters is that the value is
            // bounded at all, because that is what a sender scales down to.
            assert!(width >= 1920 && height >= 1080, "{width}x{height}");
        }
        // decodebin picks its decoder while running and advertises no frame size of its own, and a
        // decoder that is not installed cannot be asked; both mean "no known limit".
        assert_eq!(decoder_frame_limits("decodebin"), None);
        assert_eq!(decoder_frame_limits("screen_mirror_missing_element"), None);
    }

    #[test]
    fn native_video_caps_do_not_force_a_receiver_aspect_ratio() {
        let caps = video_caps(30, None, None, true);
        assert!(!caps.contains("width="));
        assert!(!caps.contains("height="));
    }
}
