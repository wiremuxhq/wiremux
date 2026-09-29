//! Thin CLI: map a request, profile validate/ingest, auth login/status, optional local proxy.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use wiremux::cli::{
    EXIT_ERROR, EXIT_NOT_READY, EXIT_OK, format_status, list_cli_profiles, load_cli_profile,
    parse_wire, pick_auth_profile, run_login, token_status, validate_report,
};
use wiremux::ingest::{
    CatalogKind, IngestAction, IngestRequest, fetch_catalog_url, ingest_catalog,
};
use wiremux::{LossAction, LossReport, Wire, decode, encode, parse_profile_str};

#[derive(Parser)]
#[command(
    name = "wiremux",
    version,
    about = "Map LLM dialect requests and refresh tokens from vendor profiles."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Encode a request JSON body. Does not send HTTP.
    Map {
        /// Source dialect (`chat-completions` / `chat`, `messages`, `responses`, `gemini`, `converse`).
        #[arg(long)]
        from: String,
        /// Dest dialect.
        #[arg(long)]
        to: String,
        /// Request JSON path. `-` reads stdin. Omit to read stdin.
        #[arg(value_name = "FILE", allow_hyphen_values = true)]
        file: Option<PathBuf>,
    },
    /// Optional local HTTP proxy (source dialect -> profile target).
    Proxy {
        /// Bind address. 127.0.0.1 only.
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: String,
        /// Incoming harness dialect (`chat-completions` / `chat`, `messages`, `responses`, `gemini`, `converse`).
        #[arg(long = "from")]
        from: String,
        /// Profile id or file path.
        #[arg(long)]
        profile: String,
        /// Print LossReport on stderr per request.
        #[arg(long)]
        dump_loss: bool,
        /// Override profile `read_timeout_secs` (default 120).
        #[arg(long)]
        read_timeout_secs: Option<u64>,
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
        profile: Option<String>,
        /// Profile id or file path (same as `--profile`).
        #[arg(value_name = "PROFILE")]
        id: Option<String>,
    },
    /// Whether a token can be loaded (does not print it).
    Status {
        /// Profile id or file path.
        #[arg(long)]
        profile: Option<String>,
        /// Profile id or file path (same as `--profile`).
        #[arg(value_name = "PROFILE")]
        id: Option<String>,
    },
}

#[derive(Subcommand)]
enum ProfileCommand {
    /// Print catalog ids (shipped plus user overlay dirs).
    List,
    /// Gist lint: refuse code-exec, print resolved URLs with secrets redacted.
    Validate {
        /// Profile id or file path.
        path: PathBuf,
    },
    /// Write user-dir TOML from a public vendor catalog (not shipped presets).
    Ingest {
        /// `models-dev` (default) or `litellm`.
        #[arg(long, default_value = "models-dev")]
        source: String,
        /// Local catalog JSON. Do not fetch.
        #[arg(long)]
        from_file: Option<PathBuf>,
        /// Catalog ids. Repeat. Default: groq, deepseek, togetherai, fireworks-ai, mistral, cerebras.
        #[arg(long = "vendor")]
        vendors: Vec<String>,
        /// Every openai-compat row the catalog can resolve (skips Azure/Bedrock/Vertex/Copilot).
        #[arg(long)]
        all_compatible: bool,
        /// Destination directory (default: user overlay dir).
        #[arg(long)]
        dir: Option<PathBuf>,
        /// Print paths; do not write.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing file or a shipped id.
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = match cli.command {
        Command::Profile {
            command: ProfileCommand::List,
        } => cmd_list(),
        Command::Profile {
            command: ProfileCommand::Validate { path },
        } => cmd_validate(&path),
        Command::Profile {
            command:
                ProfileCommand::Ingest {
                    source,
                    from_file,
                    vendors,
                    all_compatible,
                    dir,
                    dry_run,
                    force,
                },
        } => {
            cmd_ingest(
                &source,
                from_file.as_deref(),
                &vendors,
                all_compatible,
                dir.as_deref(),
                dry_run,
                force,
            )
            .await
        }
        Command::Auth {
            command: AuthCommand::Login { profile, id },
        } => match pick_auth_profile(profile, id) {
            Ok(profile) => cmd_login(&profile).await,
            Err(err) => {
                eprintln!("{err}");
                EXIT_ERROR
            }
        },
        Command::Auth {
            command: AuthCommand::Status { profile, id },
        } => match pick_auth_profile(profile, id) {
            Ok(profile) => cmd_status(&profile),
            Err(err) => {
                eprintln!("{err}");
                EXIT_ERROR
            }
        },
        Command::Map { from, to, file } => cmd_map(&from, &to, file.as_deref()),
        Command::Proxy {
            listen,
            from,
            profile,
            dump_loss,
            read_timeout_secs,
        } => cmd_proxy(&listen, &from, &profile, dump_loss, read_timeout_secs).await,
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

fn cmd_list() -> i32 {
    match list_cli_profiles() {
        Ok(ids) => {
            for id in ids {
                println!("{id}");
            }
            EXIT_OK
        }
        Err(err) => {
            eprintln!("{err}");
            EXIT_ERROR
        }
    }
}

async fn cmd_ingest(
    source: &str,
    from_file: Option<&std::path::Path>,
    vendors: &[String],
    all_compatible: bool,
    dir: Option<&std::path::Path>,
    dry_run: bool,
    force: bool,
) -> i32 {
    let kind = match CatalogKind::parse_name(source) {
        Ok(kind) => kind,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let text = match from_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("{}: {err}", path.display());
                return EXIT_ERROR;
            }
        },
        None => match fetch_catalog_url(kind.default_url()).await {
            Ok(text) => text,
            Err(err) => {
                eprintln!("{err}");
                return EXIT_ERROR;
            }
        },
    };
    let req = IngestRequest {
        kind,
        vendors: vendors.to_vec(),
        all_compatible,
        dir: dir.map(PathBuf::from),
        dry_run,
        force,
    };
    match ingest_catalog(&text, &req) {
        Ok(report) => {
            for action in &report.actions {
                match action {
                    IngestAction::Wrote(path) => println!("wrote {}", path.display()),
                    IngestAction::DryRun(path) => println!("dry-run {}", path.display()),
                    IngestAction::Skipped { vendor, reason } => {
                        println!("skip {vendor}: {reason}");
                    }
                }
            }
            EXIT_OK
        }
        Err(err) => {
            eprintln!("{err}");
            EXIT_ERROR
        }
    }
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

async fn cmd_proxy(
    listen: &str,
    from: &str,
    profile_arg: &str,
    dump_loss: bool,
    read_timeout_secs: Option<u64>,
) -> i32 {
    let from = match parse_wire(from) {
        Ok(w) => w,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let mut profile = match load_cli_profile(profile_arg) {
        Ok(p) => p,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    if let Some(secs) = read_timeout_secs.filter(|&s| s > 0) {
        profile.http.read_timeout_secs = Some(secs);
    }
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

fn cmd_map(from: &str, to: &str, file: Option<&Path>) -> i32 {
    let from = match parse_flag_wire("--from", from) {
        Ok(wire) => wire,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let to = match parse_flag_wire("--to", to) {
        Ok(wire) => wire,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let bytes = match read_map_input(file) {
        Ok(bytes) => bytes,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let (ir, decode_loss) = match decode(from, &bytes) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    print_loss(&decode_loss);
    // Vendor quirks come from this profile. The command does not send the body.
    let profile = match parse_profile_str(&format!(
        "schema_version = 1\nid = \"map\"\nwire = \"{}\"\n",
        to.as_str()
    )) {
        Ok(profile) => profile,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    let (body, encode_loss) = match encode(to, &ir, &profile) {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("{err}");
            return EXIT_ERROR;
        }
    };
    print_loss(&encode_loss);
    let mut stdout = std::io::stdout().lock();
    if stdout.write_all(&body).is_err() || stdout.write_all(b"\n").is_err() {
        return EXIT_ERROR;
    }
    if loss_is_hard(&decode_loss) || loss_is_hard(&encode_loss) {
        EXIT_ERROR
    } else {
        EXIT_OK
    }
}

fn parse_flag_wire(flag: &str, value: &str) -> Result<Wire, String> {
    parse_wire(value).map_err(|err| {
        if flag == "--from" {
            err
        } else {
            err.replacen("--from", flag, 1)
        }
    })
}

fn read_map_input(file: Option<&Path>) -> Result<Vec<u8>, String> {
    match file {
        None => read_stdin(),
        Some(path) if path.as_os_str() == "-" => read_stdin(),
        Some(path) => std::fs::read(path).map_err(|err| format!("{}: {err}", path.display())),
    }
}

fn read_stdin() -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    std::io::stdin()
        .read_to_end(&mut bytes)
        .map_err(|err| format!("stdin: {err}"))?;
    Ok(bytes)
}

fn print_loss(report: &LossReport) {
    for event in &report.events {
        eprintln!(
            "{} {}: {}",
            loss_action_label(event.action),
            event.path,
            event.detail
        );
    }
}

fn loss_is_hard(report: &LossReport) -> bool {
    report
        .events
        .iter()
        .any(|event| matches!(event.action, LossAction::HardError))
}

fn loss_action_label(action: LossAction) -> &'static str {
    match action {
        LossAction::Preserve => "preserve",
        LossAction::Degrade => "degrade",
        LossAction::Drop => "drop",
        LossAction::HardError => "harderror",
        _ => "loss",
    }
}
