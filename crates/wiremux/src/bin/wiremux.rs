//! Thin CLI: profile validate, auth login/status, optional local proxy.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wiremux::cli::{
    EXIT_ERROR, EXIT_NOT_READY, EXIT_OK, format_status, load_cli_profile, parse_wire, run_login,
    token_status, validate_report,
};

#[derive(Parser)]
#[command(name = "wiremux", version, about = "Reserved.")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Optional local HTTP proxy (source dialect -> profile target).
    Proxy {
        /// Bind address. 127.0.0.1 only.
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: String,
        /// Incoming harness dialect.
        #[arg(long = "from")]
        from: String,
        /// Profile id or file path.
        #[arg(long)]
        profile: String,
        /// Print LossReport on stderr per request.
        #[arg(long)]
        dump_loss: bool,
    },
    /// Credential login and status.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Profile catalog helpers.
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Start the profile login flow.
    Login {
        /// Profile id or file path.
        #[arg(long)]
        profile: String,
    },
    /// Whether a token can be loaded (does not print it).
    Status {
        /// Profile id or file path.
        #[arg(long)]
        profile: String,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Gist lint: refuse code-exec, print resolved URLs with secrets redacted.
    Validate {
        /// Profile id or file path.
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Profile {
            command: ProfileCommand::Validate { path },
        } => cmd_validate(&path),
        Command::Auth {
            command: AuthCommand::Login { profile },
        } => cmd_login(&profile).await,
        Command::Auth {
            command: AuthCommand::Status { profile },
        } => cmd_status(&profile),
        Command::Proxy {
            listen,
            from,
            profile,
            dump_loss,
        } => cmd_proxy(&listen, &from, &profile, dump_loss).await,
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn cmd_validate(path: &std::path::Path) -> i32 {
    let arg = path.to_string_lossy();
    match load_cli_profile(&arg) {
        Ok(profile) => {
            println!("{}", validate_report(&profile));
            EXIT_OK
        }
        Err(err) => {
            eprintln!("{err}");
            EXIT_ERROR
        }
    }
}

async fn cmd_login(profile_arg: &str) -> i32 {
    match load_cli_profile(profile_arg) {
        Ok(profile) => run_login(&profile).await,
        Err(err) => {
            eprintln!("{err}");
            EXIT_ERROR
        }
    }
}

fn cmd_status(profile_arg: &str) -> i32 {
    match load_cli_profile(profile_arg) {
        Ok(profile) => {
            let status = token_status(&profile);
            println!("{}", format_status(&status));
            if status.available {
                EXIT_OK
            } else {
                EXIT_NOT_READY
            }
        }
        Err(err) => {
            eprintln!("{err}");
            EXIT_ERROR
        }
    }
}

async fn cmd_proxy(listen: &str, from: &str, profile_arg: &str, dump_loss: bool) -> i32 {
    let from = match parse_wire(from) {
        Ok(w) => w,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let profile = match load_cli_profile(profile_arg) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    #[cfg(feature = "proxy")]
    {
        if let Err(err) = wiremux::proxy::run(listen, from, profile, dump_loss).await {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
        EXIT_OK
    }
    #[cfg(not(feature = "proxy"))]
    {
        let _ = (listen, from, profile, dump_loss);
        eprintln!("wiremux was built without the proxy feature");
        EXIT_NOT_READY
    }
}
