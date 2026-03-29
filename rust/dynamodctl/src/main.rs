/// dynamodctl: CLI tool for controlling the dynamod init system.
///
/// Communicates with dynamod-svmgr over the control socket at
/// /run/dynamod/control.sock.
mod commands;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "dynamodctl", about = "Control the dynamod init system")]
struct Cli {
    /// Path to the control socket
    #[arg(long, default_value = dynamod_common::paths::CONTROL_SOCK)]
    socket: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start a service
    Start {
        /// Service name
        name: String,
    },
    /// Stop a service
    Stop {
        /// Service name
        name: String,
    },
    /// Restart a service
    Restart {
        /// Service name
        name: String,
    },
    /// Show status of a service
    Status {
        /// Service name
        name: String,
    },
    /// List all services
    List,
    /// Show the supervisor tree
    Tree,
    /// Shut down the system
    Shutdown {
        /// Shutdown type: poweroff, reboot, or halt
        #[arg(default_value = "poweroff")]
        kind: String,
    },
}

fn main() {
    let cli = Cli::parse();
    let socket_path = &cli.socket;

    let result = match cli.command {
        Command::Start { name } => commands::start(socket_path, &name),
        Command::Stop { name } => commands::stop(socket_path, &name),
        Command::Restart { name } => commands::restart(socket_path, &name),
        Command::Status { name } => commands::status(socket_path, &name),
        Command::List => commands::list(socket_path),
        Command::Tree => commands::tree(socket_path),
        Command::Shutdown { kind } => commands::shutdown(socket_path, &kind),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
