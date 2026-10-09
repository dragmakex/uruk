//! The `uruk` binary: the web server launcher.
//!
//! The browser is the only frontend, so `uruk serve` is the only command.
//! Research operations — starting, steering, and stopping runs, reports,
//! passage search — live in the web API (`src/web`, docs/WEB.md); the
//! engine itself is the `uruk` library crate.
//!
//! Configuration splits by lifetime: the project directory and bind
//! address are flags, while the model provider (`URUK_PROVIDER_URL`,
//! `URUK_MODEL`, `URUK_API_KEY`) comes from the environment or the
//! project's `.env`, keeping model selection outside research policy
//! (SPEC §13).

use clap::{Parser, Subcommand};
use uruk::{Error, Result};

#[derive(Parser, Debug)]
#[command(
    name = "uruk",
    version,
    about = "An autonomous research engine whose output is evidence-backed findings.\n\
             The browser is the frontend: `uruk serve` runs the local web API."
)]
struct Cli {
    /// Project directory holding `.uruk/state.sqlite` and `runs/`.
    #[arg(long, global = true, default_value = ".")]
    project: String,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Serve the local web API (and the Next.js frontend's backend).
    ///
    /// SECURITY: there is no authentication. Keep the default loopback bind
    /// unless an authenticating reverse proxy fronts it (docs/WEB.md).
    Serve {
        /// Address to bind, loopback by default.
        #[arg(long, default_value = "127.0.0.1:7913")]
        bind: String,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("URUK_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("uruk=info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    load_dotenv(std::path::Path::new(&cli.project));

    if let Err(e) = dispatch(cli).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Serve { bind } => {
            let bind: std::net::SocketAddr = bind
                .parse()
                .map_err(|e| Error::validation(format!("bad --bind address {bind:?}: {e}")))?;
            // Strictly validated so a typo fails startup instead of
            // silently weakening the browser-identity cookie.
            let cookie_secure = uruk::web::cookie_secure_from_env(
                std::env::var("URUK_COOKIE_SECURE").ok().as_deref(),
            )?;
            uruk::web::serve(uruk::web::ServeOptions {
                bind,
                project: std::path::PathBuf::from(&cli.project),
                config: uruk::web::WebConfig {
                    cookie_secure,
                    ..uruk::web::WebConfig::default()
                },
            })
            .await
        }
    }
}

/// Apply `<project>/.env` to the process environment. Variables already set
/// in the real environment win, so a shell override still works. A missing
/// file is not an error.
fn load_dotenv(project: &std::path::Path) {
    let Ok(text) = std::fs::read_to_string(project.join(".env")) else {
        return;
    };
    for (key, value) in parse_dotenv(&text) {
        if std::env::var_os(&key).is_none() {
            // SAFETY: called once at startup, before any other thread reads the
            // environment.
            unsafe { std::env::set_var(&key, &value) };
        }
    }
}

/// `KEY=value` lines; blanks, `#` comments, an `export ` prefix and one pair
/// of surrounding quotes are tolerated. Anything else is skipped.
fn parse_dotenv(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let line = line.strip_prefix("export ").unwrap_or(line);
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let bad = key.is_empty() || key.starts_with('#') || key.contains(char::is_whitespace);
            if bad {
                return None;
            }
            let value = value.trim();
            let value = match value.as_bytes() {
                [q @ (b'"' | b'\''), .., last] if last == q => &value[1..value.len() - 1],
                _ => value,
            };
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Cli, parse_dotenv};
    use clap::CommandFactory;

    /// The parser-level statement of the web-only product decision: the
    /// binary offers `serve` and nothing else.
    #[test]
    fn serve_is_the_only_subcommand() {
        let cmd = Cli::command();
        cmd.clone().debug_assert();
        let subcommands: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        assert_eq!(subcommands, ["serve"]);
    }

    #[test]
    fn dotenv_lines_parse_and_junk_is_skipped() {
        let text = "# comment\n\nexport URUK_MODEL=qwen3.5:9b\nURUK_PROVIDER_URL = \"http://localhost:11434/v1\"\nKEY='x=y'\nnot a pair\n=novalue\n";
        assert_eq!(
            parse_dotenv(text),
            vec![
                ("URUK_MODEL".into(), "qwen3.5:9b".into()),
                (
                    "URUK_PROVIDER_URL".into(),
                    "http://localhost:11434/v1".into()
                ),
                ("KEY".into(), "x=y".into()),
            ]
        );
    }
}
