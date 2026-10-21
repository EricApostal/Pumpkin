//! `mctrace`: the CLI of the recording TCP proxy in `pumpkin_tester::mctrace`.
//!
//! ```text
//! usage:
//!   mctrace --listen <host:port> --upstream <host:port> --out <trace.jsonl>
//!   mctrace decode --trace <trace.jsonl> [--dir c2s|s2c] [--connection <n>]
//! ```
//!
//! The proxy records every frame of every connection that goes through it, both directions, one
//! JSON line each (`pumpkin_tester::mctrace` documents the schema). `decode` replays a trace into
//! the protocol decoders this repository already has and writes one line per packet.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use pumpkin_tester::mctrace::{self, DecodeOptions, Dir, ProxyOptions};

const USAGE: &str = "usage:\n  \
    mctrace --listen <host:port> --upstream <host:port> --out <trace.jsonl>\n  \
    mctrace decode --trace <trace.jsonl> [--dir c2s|s2c] [--connection <n>]";

/// One value after a flag, or the usage.
#[expect(
    clippy::print_stderr,
    reason = "a CLI: a usage error has nowhere else to go"
)]
fn value<'a>(args: &'a [String], index: usize, flag: &str) -> Option<&'a str> {
    match args.get(index + 1) {
        Some(value) => Some(value.as_str()),
        None => {
            eprintln!("{flag} needs a value\n{USAGE}");
            None
        }
    }
}

#[expect(
    clippy::print_stderr,
    reason = "a CLI: a usage error has nowhere else to go"
)]
fn parse_address(args: &[String], index: usize, flag: &str) -> Option<SocketAddr> {
    let value = value(args, index, flag)?;
    match value.parse() {
        Ok(address) => Some(address),
        Err(err) => {
            eprintln!("{flag} {value}: {err}");
            None
        }
    }
}

/// The proxy invocation, or `None` if the arguments did not describe one.
#[expect(
    clippy::print_stderr,
    reason = "a CLI: a usage error has nowhere else to go"
)]
fn parse_proxy(args: &[String]) -> Option<ProxyOptions> {
    let (mut listen, mut upstream, mut out) = (None, None, None);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--listen" => {
                listen = Some(parse_address(args, index, "--listen")?);
                index += 2;
            }
            "--upstream" => {
                upstream = Some(parse_address(args, index, "--upstream")?);
                index += 2;
            }
            "--out" => {
                out = Some(PathBuf::from(value(args, index, "--out")?));
                index += 2;
            }
            other => {
                eprintln!("unknown argument '{other}'\n{USAGE}");
                return None;
            }
        }
    }
    match (listen, upstream, out) {
        (Some(listen), Some(upstream), Some(out)) => Some(ProxyOptions {
            listen,
            upstream,
            out,
        }),
        _ => {
            eprintln!("--listen, --upstream and --out are all required\n{USAGE}");
            None
        }
    }
}

/// The decode invocation, or `None` if the arguments did not describe one.
#[expect(
    clippy::print_stderr,
    reason = "a CLI: a usage error has nowhere else to go"
)]
fn parse_decode(args: &[String]) -> Option<DecodeOptions> {
    let (mut trace, mut dir, mut connection) = (None, None, None);
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--trace" => {
                trace = Some(PathBuf::from(value(args, index, "--trace")?));
                index += 2;
            }
            "--dir" => {
                let name = value(args, index, "--dir")?;
                match Dir::from_name(name) {
                    Some(parsed) => dir = Some(parsed),
                    None => {
                        eprintln!("--dir {name}: expected c2s or s2c");
                        return None;
                    }
                }
                index += 2;
            }
            "--connection" => {
                let number = value(args, index, "--connection")?;
                match number.parse() {
                    Ok(parsed) => connection = Some(parsed),
                    Err(err) => {
                        eprintln!("--connection {number}: {err}");
                        return None;
                    }
                }
                index += 2;
            }
            other => {
                eprintln!("unknown argument '{other}'\n{USAGE}");
                return None;
            }
        }
    }
    match trace {
        Some(trace) => Some(DecodeOptions {
            trace,
            dir,
            connection,
        }),
        None => {
            eprintln!("--trace is required\n{USAGE}");
            None
        }
    }
}

#[expect(
    clippy::print_stderr,
    clippy::print_stdout,
    reason = "a CLI: a usage error, a failure and --help have nowhere else to go"
)]
#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("decode") => {
            let Some(options) = parse_decode(&args[1..]) else {
                return ExitCode::FAILURE;
            };
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            match mctrace::decode(&options, &mut lock).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("mctrace decode FAILED: {err}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("--help" | "help") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        _ => {
            let Some(options) = parse_proxy(&args) else {
                return ExitCode::FAILURE;
            };
            match mctrace::run(options).await {
                Ok(()) => ExitCode::SUCCESS,
                Err(err) => {
                    eprintln!("mctrace FAILED: {err}");
                    ExitCode::FAILURE
                }
            }
        }
    }
}