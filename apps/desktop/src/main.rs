#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod autostart;
mod config;
mod control;
mod lan;
mod pipeline;
mod tray_app;

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
    /// Print the generated GStreamer pipeline without running it.
    Print(PrintArgs),
    /// Run an explicit gst-launch-style pipeline.
    Run(RunArgs),
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
    pipeline: String,
}

#[derive(Args, Debug)]
struct DiscoverArgs {
    /// Discovery timeout in milliseconds.
    #[arg(long, default_value_t = 3000)]
    timeout_ms: u64,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    gstreamer::init().context("failed to initialize GStreamer")?;

    match cli.command.unwrap_or(Command::Tray) {
        Command::Tray => tray_app::run(),
        Command::Send(args) => {
            let args = lan::resolve_sender_args(args)?;
            let _control = control::ControlServer::start()?;
            let pipeline = pipeline::build_sender_pipeline(&args)?;
            eprintln!("pipeline: {pipeline}");
            run_pipeline(&pipeline)
        }
        Command::Recv(args) => {
            let pipeline = pipeline::build_receiver_pipeline(&args)?;
            eprintln!("pipeline: {pipeline}");
            run_pipeline(&pipeline)
        }
        Command::Probe => {
            probe_elements();
            Ok(())
        }
        Command::Discover(args) => {
            let peers = lan::discover_receivers(std::time::Duration::from_millis(args.timeout_ms))?;
            for peer in peers {
                println!(
                    "{} {}:{} ({})",
                    peer.announcement.device_name,
                    peer.address,
                    peer.announcement.stream_port,
                    peer.announcement.instance_id
                );
            }
            Ok(())
        }
        Command::Print(args) => {
            let pipeline = match args.command {
                PrintCommand::Send(send_args) => pipeline::build_sender_pipeline(&send_args)?,
                PrintCommand::Recv(recv_args) => pipeline::build_receiver_pipeline(&recv_args)?,
            };
            println!("{pipeline}");
            Ok(())
        }
        Command::Run(args) => run_pipeline(&args.pipeline),
    }
}
