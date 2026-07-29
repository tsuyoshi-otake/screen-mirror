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

    /// Decoder to use. auto prefers D3D11 GPU decode.
    #[arg(long, value_enum, default_value_t = Decoder::Auto)]
    pub decoder: Decoder,

    /// Video sink to use. auto prefers D3D11 GPU rendering.
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
    let video = build_sender_video_pipeline(args)?;
    if !args.audio_enabled {
        return Ok(video);
    }

    Ok(format!("{video} {}", build_sender_audio_pipeline(args)?))
}

pub fn build_sender_video_pipeline(args: &SendArgs) -> Result<String> {
    build_sender_video_pipeline_for(args, None)
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

    let gpu = crate::gpu::resolve(&args.gpu);
    if let Some(gpu) = gpu.as_ref() {
        crate::logging::append(format!("sender GPU selected: {}", gpu.summary()));
    }
    let encoder = select_encoder(args.encoder, args.allow_software_encoder, gpu.as_ref())?;
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
    let encoder_accepts_d3d11 = encoder.supports_d3d11_input();
    // Frames captured on one GPU cannot stay in D3D11 memory while another GPU encodes them, so
    // that combination goes through system memory and lets the encoder upload on its own device.
    let same_gpu_as_capture = match encoder.adapter_luid {
        Some(encoder_luid) => {
            capture_adapter(capture_target.as_ref(), args.monitor_index).map(|adapter| adapter.luid)
                == Some(encoder_luid)
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
         ! rtph264pay pt=96 mtu={} config-interval=-1 aggregate-mode=zero-latency \
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
    let video = build_receiver_video_pipeline(args)?;
    if !args.audio_enabled {
        return Ok(video);
    }

    Ok(format!("{video} {}", build_receiver_audio_pipeline(args)?))
}

pub fn build_receiver_video_pipeline(args: &RecvArgs) -> Result<String> {
    ensure_positive("jitter-ms", args.jitter_ms)?;
    ensure_positive("udp-buffer-size", args.udp_buffer_size)?;
    ensure_positive("mtu", args.mtu)?;
    ensure_positive("jitter-faststart-packets", args.jitter_faststart_packets)?;
    ensure_positive("jitter-max-dropout-ms", args.jitter_max_dropout_ms)?;
    ensure_positive("jitter-max-misorder-ms", args.jitter_max_misorder_ms)?;

    let gpu = crate::gpu::resolve(&args.gpu);
    if let Some(gpu) = gpu.as_ref() {
        crate::logging::append(format!("receiver GPU selected: {}", gpu.summary()));
    }
    let decoder = select_decoder(args.decoder, gpu.as_ref())?;
    let sink = select_sink(args.sink, args.fullscreen, gpu.as_ref())?;
    crate::logging::append(format!("receiver decoder={decoder} sink={sink}"));

    let video = format!(
        "udpsrc name=receiver_video_src port={} buffer-size={} mtu={} retrieve-sender-address=false timeout=0 caps=\"application/x-rtp,media=(string)video,clock-rate=(int)90000,encoding-name=(string)H264,payload=(int)96,packetization-mode=(string)1\" \
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

    Ok(video)
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
    run_pipeline_until_stop(description, stop_rx)
}

fn run_pipeline_until_stop(description: &str, stop_rx: mpsc::Receiver<()>) -> Result<()> {
    let element = gst::parse::launch(description).context("failed to parse GStreamer pipeline")?;
    let pipeline = element
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("pipeline description did not create a GstPipeline"))?;
    let bus = pipeline.bus().context("pipeline has no bus")?;
    arm_receiver_timeout_after_first_packet(&pipeline);

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

fn arm_receiver_timeout_after_first_packet(pipeline: &gst::Pipeline) {
    let Some(source) = pipeline.by_name("receiver_video_src") else {
        return;
    };
    let Some(pad) = source.static_pad("src") else {
        return;
    };
    let source_for_probe = source.clone();
    pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, _info| {
        source_for_probe.set_property("timeout", 3_000_000_000_u64);
        crate::logging::append("receiver video packets started; disconnect timeout armed");
        gst::PadProbeReturn::Remove
    });
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
        match self.family {
            EncoderFamily::Nvidia => nvidia_encoder_chain(element, bitrate, fps, nvidia_tuning),
            EncoderFamily::Amf => format!(
                "{element} bitrate={bitrate} max-bitrate={bitrate} gop-size={fps} usage=ultra-low-latency preset=speed rate-control=cbr aud=false \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
            EncoderFamily::MediaFoundation => format!(
                "{element} bitrate={bitrate} max-bitrate={bitrate} gop-size={fps} bframes=0 low-latency=true rc-mode=cbr quality-vs-speed=0 \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
            EncoderFamily::QuickSync => format!(
                "{element} bitrate={bitrate} gop-size={fps} b-frames=0 rc-lookahead=0 rate-control=cbr \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
            EncoderFamily::X264 => format!(
                "{element} bitrate={bitrate} speed-preset=ultrafast tune=zerolatency key-int-max={fps} bframes=0 sliced-threads=true byte-stream=true \
                 ! video/x-h264,stream-format=byte-stream,alignment=au,profile=constrained-baseline"
            ),
        }
    }
}

fn nvidia_encoder_chain(element: &str, bitrate: u32, fps: u32, tuning: NvidiaTuning) -> String {
    let tuning = resolve_nvidia_tuning(tuning);
    let vbv_buffer_size = (bitrate / fps.max(1)).max(128);
    let extra = match tuning {
        NvidiaTuning::Rtx => " spatial-aq=true temporal-aq=true aq-strength=8",
        NvidiaTuning::Gtx | NvidiaTuning::LowLatency | NvidiaTuning::Auto => "",
    };

    format!(
        "{element} bitrate={bitrate} max-bitrate={bitrate} vbv-buffer-size={vbv_buffer_size} \
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

fn select_decoder(requested: Decoder, gpu: Option<&crate::gpu::GpuAdapter>) -> Result<String> {
    match requested {
        Decoder::Auto | Decoder::D3d11 => {
            if let Some(decoder) = gpu.and_then(d3d11_decoder_on_gpu) {
                return Ok(decoder);
            }
            match requested {
                Decoder::D3d11 => require_element("d3d11h264dec", "d3d11h264dec".to_string()),
                _ => require_element("decodebin", "decodebin".to_string()),
            }
        }
        Decoder::Avdec => require_element("avdec_h264", "avdec_h264".to_string()),
    }
}

fn d3d11_decoder_on_gpu(gpu: &crate::gpu::GpuAdapter) -> Option<String> {
    let decoder = family_elements(gst::ElementFactoryType::DECODER, |name| {
        name.starts_with("d3d11") && name.contains("h264") && name.ends_with("dec")
    })
    .into_iter()
    .find(|element| element_adapter_luid(element) == Some(gpu.luid));

    if decoder.is_none() {
        crate::logging::append(format!(
            "no d3d11 H.264 decoder is bound to GPU {}; using the default decoder",
            gpu.description
        ));
    }
    decoder
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
            "d3d11videosink{adapter} sync=false qos=true fullscreen-toggle-mode=property fullscreen=true"
        )
    } else {
        format!("d3d11videosink{adapter} sync=false qos=true")
    };

    match requested {
        Sink::Auto => {
            if has_element("d3d11videosink") {
                Ok(d3d11_sink)
            } else {
                Ok("autovideosink sync=false".to_string())
            }
        }
        Sink::D3d11 => require_element("d3d11videosink", d3d11_sink),
        Sink::AutoVideo => Ok("autovideosink sync=false".to_string()),
    }
}

/// Registered element factory names matching a predicate, shortest name first so the family's
/// base (device-0) element wins ties.
fn family_elements(
    factory_type: gst::ElementFactoryType,
    predicate: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut names: Vec<String> =
        gst::ElementFactory::factories_with_type(factory_type, gst::Rank::MARGINAL)
            .iter()
            .map(|factory| factory.name().to_string())
            .filter(|name| predicate(name))
            .collect();
    names.sort_by(|left, right| left.len().cmp(&right.len()).then_with(|| left.cmp(right)));
    names
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn native_video_caps_do_not_force_a_receiver_aspect_ratio() {
        let caps = video_caps(30, None, None, true);
        assert!(!caps.contains("width="));
        assert!(!caps.contains("height="));
    }
}
