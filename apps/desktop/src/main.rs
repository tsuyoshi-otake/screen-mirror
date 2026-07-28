#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod config;
mod console;
mod control;
mod diagnostics;
mod lan;
mod logging;
mod monitors;
mod pipeline;
mod power;
mod process;
mod receiver_window;
mod single_instance;
mod tray_app;
#[cfg(windows)]
mod tray_menu_owner;
mod updater;
mod vdd;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

use crate::pipeline::{probe_elements, run_pipeline};

#[derive(Parser, Debug)]
#[command(name = "screen-mirror")]
#[command(about = "Low-latency Windows screen mirroring over RTP/H.264")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run as a task-tray desktop app.
    Tray,
    /// Capture this Windows desktop and send it to a receiver.
    Send(pipeline::SendArgs),
    /// Receive a stream and render it locally.
    Recv(pipeline::RecvArgs),
    /// Print available GStreamer elements used by this app.
    Probe,
    /// Discover receivers on the local network.
    Discover(DiscoverArgs),
    /// List Windows displays and likely VDD/SuperDisplay-style virtual monitors.
    Monitors,
    /// Print the generated GStreamer pipeline without running it.
    Print(PrintArgs),
    /// Run an explicit gst-launch-style pipeline.
    Run(RunArgs),
    /// Manage the bundled virtual display driver. Used internally for the elevated step.
    #[command(hide = true)]
    Vdd(VddArgs),
}

#[derive(Args, Debug)]
struct VddArgs {
    /// Which driver action to perform.
    #[arg(long, value_enum)]
    action: vdd::VddAction,

    /// Number of virtual monitors to expose, for --action set-count.
    #[arg(long, default_value_t = 1)]
    count: u32,
}

#[derive(Subcommand, Debug)]
enum PrintCommand {
    Send(pipeline::SendArgs),
    Recv(pipeline::RecvArgs),
}

#[derive(Args, Debug)]
struct PrintArgs {
    #[command(subcommand)]
    command: PrintCommand,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Full gst-launch-style pipeline description.
    #[arg(trailing_var_arg = true)]
    pipeline: Vec<String>,
}

#[derive(Args, Debug)]
struct DiscoverArgs {
    /// Discovery timeout in milliseconds.
    #[arg(long, default_value_t = 3000)]
    timeout_ms: u64,

    /// Four-digit PIN used to filter receiver discovery.
    #[arg(long, default_value = sm_core::discovery::DEFAULT_PIN)]
    pin: String,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    // The elevated driver step must not depend on GStreamer being loadable.
    if let Some(Command::Vdd(args)) = &cli.command {
        return vdd::apply(args.action, args.count);
    }
    let tray_mode = matches!(cli.command, None | Some(Command::Tray));
    let _instance_guard = if tray_mode {
        Some(single_instance::acquire_tray_instance()?)
    } else {
        console::attach_for_cli();
        None
    };

    configure_bundled_runtime();
    gstreamer::init().context("failed to initialize GStreamer")?;

    match cli.command.unwrap_or(Command::Tray) {
        Command::Tray => tray_app::run(),
        Command::Send(args) => {
            let args = lan::resolve_sender_args(args)?;
            let _control = control::ControlServer::start(&args.pin)?;
            let pipeline = pipeline::build_sender_pipeline(&args)?;
            eprintln!("pipeline: {pipeline}");
            run_pipeline(&pipeline)
        }
        Command::Recv(args) => {
            let _sleep_guard = power::SleepGuard::receiver();
            let _render_window = receiver_window::RenderWindowGuard::start();
            let pipeline = pipeline::build_receiver_pipeline(&args)?;
            eprintln!("pipeline: {pipeline}");
            run_pipeline(&pipeline)
        }
        Command::Probe => {
            probe_elements();
            Ok(())
        }
        Command::Discover(args) => {
            let peers = lan::discover_receivers_with_pin(
                std::time::Duration::from_millis(args.timeout_ms),
                &args.pin,
            )?;
            for peer in peers {
                console::line(format!(
                    "{} {}:{} ({})",
                    peer.announcement.device_name,
                    peer.address,
                    peer.announcement.stream_port,
                    peer.announcement.instance_id
                ));
            }
            Ok(())
        }
        Command::Monitors => {
            monitors::print_monitors();
            Ok(())
        }
        Command::Print(args) => {
            let pipeline = match args.command {
                PrintCommand::Send(send_args) => pipeline::build_sender_pipeline(&send_args)?,
                PrintCommand::Recv(recv_args) => pipeline::build_receiver_pipeline(&recv_args)?,
            };
            console::line(pipeline);
            Ok(())
        }
        Command::Run(args) => run_pipeline(&args.pipeline.join(" ")),
        // Reached through the UAC prompt; this process is already elevated.
        Command::Vdd(args) => vdd::apply(args.action, args.count),
    }
}

fn configure_bundled_runtime() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(app_dir) = exe.parent() else {
        return;
    };

    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = std::env::split_paths(&path).collect::<Vec<_>>();
    if !paths.iter().any(|path| path == app_dir) {
        paths.insert(0, app_dir.to_path_buf());
    }
    let bin_dir = app_dir.join("bin");
    if bin_dir.exists() && !paths.iter().any(|path| path == &bin_dir) {
        paths.insert(0, bin_dir);
    }
    if let Ok(joined) = std::env::join_paths(paths) {
        std::env::set_var("PATH", joined);
    }

    let plugin_path = app_dir.join("lib").join("gstreamer-1.0");
    if plugin_path.exists() && std::env::var_os("GST_PLUGIN_PATH").is_none() {
        std::env::set_var("GST_PLUGIN_PATH", plugin_path);
    }

    let scanner = app_dir
        .join("libexec")
        .join("gstreamer-1.0")
        .join("gst-plugin-scanner.exe");
    if scanner.exists() && std::env::var_os("GST_PLUGIN_SCANNER").is_none() {
        std::env::set_var("GST_PLUGIN_SCANNER", scanner);
    }
}
