//! `ename_mcp game <verb>`, aliased as `cargo game <verb>`: the launcher for agents without MCP,
//! and for humans.
//!
//! No argument-parsing crate, like `ename_xtask`: a handful of flat flags, and a parser over a
//! plain iterator a test can build.

use std::{
    collections::BTreeMap,
    hash::{BuildHasher as _, Hasher as _},
    process::ExitCode,
    time::Duration,
};

use super::{
    Caller, DEFAULT_INSTANCE, GameState, LaunchRequest, LaunchUpdate, Launcher, Project, Status,
    supervise::DEFAULT_GRACE,
};

const USAGE: &str = "\
usage: cargo game <verb> [flags]

  start    [--instance NAME] [--port N] [--env K=V]... [--features F]...
           [--window-size WxH] [--render-size WxH] [--settle] [--wait SECONDS]
  restart  [same as start] [--take-over]
  stop     [--instance NAME] [--take-over] [--grace SECONDS]
  status   [--instance NAME] [--wait SECONDS]
  owner    [--new]

Every verb takes --owner TOKEN (else ENAME_GAME_OWNER; without one the caller is the user)
and --json. An agent always passes a token: `cargo game owner --new` mints one.

Exit codes: 0 ready or stopped, 1 build failed or crashed, 2 still building or starting,
3 port in use or external game, 4 refused or invalid.";

/// `start` and `restart` wait this long by default, under the Bash tool's 120 s.
const DEFAULT_LAUNCH_WAIT: f64 = 100.0;
const FOLLOW_HINT: &str = "run `cargo game status --wait 500` to follow it";

#[derive(Debug, Clone, PartialEq)]
pub struct Invocation {
    pub verb: Verb,
    pub owner: Option<String>,
    pub json: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verb {
    Start(LaunchOptions),
    Restart(LaunchOptions),
    Stop {
        instance: String,
        take_over: bool,
        grace: f64,
    },
    Status {
        instance: String,
        wait: f64,
    },
    Owner {
        new: bool,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LaunchOptions {
    pub instance: String,
    pub port: Option<u16>,
    pub env: BTreeMap<String, String>,
    pub features: Vec<String>,
    pub window_size: Option<[u32; 2]>,
    pub render_size: Option<[u32; 2]>,
    pub settle: bool,
    pub wait: f64,
    pub take_over: bool,
}

/// Parses the words after `game`.
pub fn parse(mut args: impl Iterator<Item = String>) -> Result<Invocation, String> {
    let verb = args.next().ok_or_else(|| USAGE.to_owned())?;
    let mut owner = None;
    let mut json = false;
    let mut instance = DEFAULT_INSTANCE.to_owned();
    let mut port = None;
    let mut env = BTreeMap::new();
    let mut features = Vec::new();
    let mut window_size = None;
    let mut render_size = None;
    let mut settle = false;
    let mut wait = None;
    let mut take_over = false;
    let mut grace = DEFAULT_GRACE;
    let mut new = false;

    while let Some(arg) = args.next() {
        // `--flag=value` and `--flag value` both work.
        let (flag, inline) = match arg.split_once('=') {
            Some((flag, value)) if arg.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = || {
            inline
                .clone()
                .or_else(|| args.next())
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match flag.as_str() {
            "--owner" => owner = Some(value()?),
            "--json" => json = true,
            "--instance" => instance = value()?,
            "--port" => port = Some(number(&flag, &value()?)?),
            "--env" => {
                let pair = value()?;
                let (key, val) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("--env takes KEY=VALUE, not `{pair}`"))?;
                env.insert(key.to_owned(), val.to_owned());
            }
            "--features" => features.extend(
                value()?
                    .split([',', ' '])
                    .filter(|f| !f.is_empty())
                    .map(str::to_owned),
            ),
            "--window-size" => window_size = Some(size(&flag, &value()?)?),
            "--render-size" => render_size = Some(size(&flag, &value()?)?),
            "--settle" => settle = true,
            "--wait" => wait = Some(number::<f64>(&flag, &value()?)?),
            "--take-over" => take_over = true,
            "--grace" => grace = number(&flag, &value()?)?,
            "--new" => new = true,
            "-h" | "--help" => return Err(USAGE.to_owned()),
            other => return Err(format!("unknown flag `{other}`\n\n{USAGE}")),
        }
    }

    let launch = |wait: Option<f64>| LaunchOptions {
        instance: instance.clone(),
        port,
        env: env.clone(),
        features: features.clone(),
        window_size,
        render_size,
        settle,
        wait: wait.unwrap_or(DEFAULT_LAUNCH_WAIT),
        take_over,
    };
    let verb = match verb.as_str() {
        "start" => Verb::Start(launch(wait)),
        "restart" => Verb::Restart(launch(wait)),
        "stop" => Verb::Stop {
            instance,
            take_over,
            grace,
        },
        "status" => Verb::Status {
            instance,
            wait: wait.unwrap_or(0.0),
        },
        "owner" => Verb::Owner { new },
        "-h" | "--help" | "help" => return Err(USAGE.to_owned()),
        other => return Err(format!("unknown verb `{other}`\n\n{USAGE}")),
    };
    Ok(Invocation { verb, owner, json })
}

fn number<T: std::str::FromStr>(flag: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{flag} takes a number, not `{text}`"))
}

fn size(flag: &str, text: &str) -> Result<[u32; 2], String> {
    let (width, height) = text
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("{flag} takes WIDTHxHEIGHT, not `{text}`"))?;
    Ok([number(flag, width)?, number(flag, height)?])
}

/// Runs `ename_mcp game ...`.
pub async fn run(args: &[String]) -> ExitCode {
    let invocation = match parse(args.iter().cloned()) {
        Ok(invocation) => invocation,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(4);
        }
    };
    let owner = invocation.owner.clone().or_else(|| {
        std::env::var("ENAME_GAME_OWNER")
            .ok()
            .filter(|owner| !owner.is_empty())
    });
    if let Verb::Owner { new } = invocation.verb {
        let token = if new { Some(mint_owner()) } else { owner };
        println!("{}", token.as_deref().unwrap_or("null"));
        return ExitCode::SUCCESS;
    }

    match execute(invocation.verb, owner).await {
        Ok(status) => {
            if invocation.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&status).unwrap_or_default()
                );
            } else {
                println!("{}", status.summary());
            }
            ExitCode::from(exit_code(&status))
        }
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(4)
        }
    }
}

async fn execute(verb: Verb, owner: Option<String>) -> anyhow::Result<Status> {
    let cwd = std::env::current_dir()?;
    let launcher = Launcher::new(Project::discover(&cwd).await?);
    let caller = Caller {
        owner,
        follow_hint: FOLLOW_HINT,
    };
    match verb {
        Verb::Start(options) => launcher.start(request(options), &caller).await,
        Verb::Restart(options) => launcher.restart(request(options), &caller).await,
        Verb::Stop {
            instance,
            take_over,
            grace,
        } => launcher.stop(&instance, take_over, grace, &caller).await,
        Verb::Status { instance, wait } => launcher.status(&instance, &caller, seconds(wait)).await,
        Verb::Owner { .. } => unreachable!("handled before the launcher is needed"),
    }
}

fn request(options: LaunchOptions) -> LaunchRequest {
    let given = |used: bool| used.then_some(());
    LaunchRequest {
        instance: options.instance,
        port: options.port,
        update: LaunchUpdate {
            env: given(!options.env.is_empty()).map(|()| options.env),
            features: given(!options.features.is_empty()).map(|()| options.features),
            window_size: options.window_size,
            render_size: options.render_size,
            settle: given(options.settle).map(|()| true),
        },
        take_over: options.take_over,
        wait: seconds(options.wait),
    }
}

fn seconds(value: f64) -> Duration {
    Duration::from_secs_f64(value.clamp(0.0, 24.0 * 3600.0))
}

fn exit_code(status: &Status) -> u8 {
    if status.is_failure() && status.state != GameState::PortInUse {
        return 1;
    }
    let pending_busy = status
        .pending
        .as_ref()
        .is_some_and(|pending| pending.state.is_transient());
    match status.state {
        _ if status.state.is_transient() || pending_busy => 2,
        GameState::PortInUse | GameState::External => 3,
        _ => 0,
    }
}

/// A fresh owner token for a top-level CLI agent.
fn mint_owner() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    hasher.write_u32(std::process::id());
    format!("cli-{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_words(words: &[&str]) -> Result<Invocation, String> {
        parse(words.iter().map(|word| (*word).to_owned()))
    }

    #[test]
    fn start_takes_every_launch_flag() {
        let invocation = parse_words(&[
            "start",
            "--instance",
            "b",
            "--env",
            "AIRSHIP_DIAGNOSTICS=1",
            "--features=trace",
            "--window-size",
            "800x600",
            "--wait",
            "20",
            "--owner",
            "cli-1",
            "--json",
        ])
        .unwrap();
        assert_eq!(invocation.owner.as_deref(), Some("cli-1"));
        assert!(invocation.json);
        let Verb::Start(options) = invocation.verb else {
            panic!("not a start");
        };
        assert_eq!(options.instance, "b");
        assert_eq!(options.env["AIRSHIP_DIAGNOSTICS"], "1");
        assert_eq!(options.features, ["trace"]);
        assert_eq!(options.window_size, Some([800, 600]));
        assert_eq!(options.wait, 20.0);
    }

    #[test]
    fn defaults_follow_the_plan() {
        let Verb::Start(options) = parse_words(&["start"]).unwrap().verb else {
            panic!("not a start");
        };
        assert_eq!(options.instance, "default");
        assert_eq!(options.wait, 100.0);
        assert_eq!(
            parse_words(&["status"]).unwrap().verb,
            Verb::Status {
                instance: "default".into(),
                wait: 0.0
            }
        );
    }

    #[test]
    fn rejects_what_it_does_not_know() {
        assert!(parse_words(&["launch"]).is_err());
        assert!(parse_words(&["start", "--bogus"]).is_err());
        assert!(parse_words(&["start", "--env", "NOEQUALS"]).is_err());
        assert!(parse_words(&[]).is_err());
    }
}
