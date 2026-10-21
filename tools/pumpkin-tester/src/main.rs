mod bot;
mod bot_tests;
mod gametests;
mod harness;

use std::{net::SocketAddr, path::PathBuf, process::ExitCode, sync::Arc, time::Instant};

use pumpkin_core::server::Server;
use pumpkin_gametest::GameTestRunner;
use pumpkin_tester::client_trace::ClientOptions;
use tokio::net::TcpListener;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// `pumpkin-tester client --addr <host:port> --name <name> --trace <path> [--neoforge] [--mirror]`
///
/// Joins a running server as a protocol-777 client and writes every packet it receives to
/// `--trace` as one JSON line per packet (`src/client_trace.rs`), so a test harness in another
/// repository can observe a server it did not boot itself.
///
/// `--light-at x,y,z` (repeatable) additionally writes the sky and block light of that cell out of
/// every chunk packet that contains it (`kind":"chunk_light_cell"`), with the section kind the
/// value came from; `--min-y` is the dimension's minimum Y (default `-64`, the overworld), which
/// the chunk packet does not carry and which maps a cell to its light section. Every chunk packet
/// also gets a `chunk_light` line (masks plus a digest of the arrays) and a `chunk_heightmap` line
/// per heightmap, so two sends can be compared byte for byte without dumping 2 KiB per section.
/// A chunk payload this reader cannot walk gets a `chunk_undecodable` line with the reason
/// instead of silence, and `--dump-chunks <dir>` writes every chunk payload out as raw bytes.
const CLIENT_USAGE: &str =
    "usage: pumpkin-tester client --addr <host:port> --name <name> --trace <path> \
     [--neoforge] [--mirror] [--query <channels.json>] [--light-at <x,y,z>]... [--min-y <y>] \
     [--dump-chunks <dir>]";

#[expect(
    clippy::print_stderr,
    reason = "a CLI: a usage error has nowhere else to go"
)]
fn parse_client_options(args: &[String]) -> Option<ClientOptions> {
    let mut addr = None;
    let mut name = None;
    let mut trace = None;
    let mut neoforge = false;
    let mut mirror = false;
    let mut query: Option<PathBuf> = None;
    let mut light_at = Vec::new();
    let mut min_y = -64;
    let mut dump_chunks = None;

    let mut index = 0;
    while index < args.len() {
        let argument = args[index].as_str();
        match argument {
            "--addr" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--addr needs a value\n{CLIENT_USAGE}");
                    return None;
                };
                match value.parse::<SocketAddr>() {
                    Ok(parsed) => addr = Some(parsed),
                    Err(err) => {
                        eprintln!("--addr {value}: {err}");
                        return None;
                    }
                }
                index += 2;
            }
            "--name" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--name needs a value\n{CLIENT_USAGE}");
                    return None;
                };
                name = Some(value.clone());
                index += 2;
            }
            "--trace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--trace needs a value\n{CLIENT_USAGE}");
                    return None;
                };
                trace = Some(PathBuf::from(value));
                index += 2;
            }
            "--light-at" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--light-at needs x,y,z\n{CLIENT_USAGE}");
                    return None;
                };
                match parse_block_position(value) {
                    Some(position) => light_at.push(position),
                    None => {
                        eprintln!("--light-at {value}: expected x,y,z");
                        return None;
                    }
                }
                index += 2;
            }
            "--min-y" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--min-y needs a value\n{CLIENT_USAGE}");
                    return None;
                };
                match value.parse::<i32>() {
                    Ok(parsed) => min_y = parsed,
                    Err(err) => {
                        eprintln!("--min-y {value}: {err}");
                        return None;
                    }
                }
                index += 2;
            }
            "--dump-chunks" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--dump-chunks needs a directory\n{CLIENT_USAGE}");
                    return None;
                };
                let dir = PathBuf::from(value);
                if let Err(err) = std::fs::create_dir_all(&dir) {
                    eprintln!("--dump-chunks {value}: {err}");
                    return None;
                }
                dump_chunks = Some(dir);
                index += 2;
            }
            "--neoforge" => {
                neoforge = true;
                index += 1;
            }
            "--query" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("--query needs a path\n{CLIENT_USAGE}");
                    return None;
                };
                // Supplying the client's own channel set is the `NeoForge` handshake.
                neoforge = true;
                query = Some(PathBuf::from(value));
                index += 2;
            }
            "--mirror" => {
                // Mirroring is the `NeoForge` handshake derived from the server, so it implies it.
                neoforge = true;
                mirror = true;
                index += 1;
            }
            other => {
                eprintln!("unknown argument '{other}'\n{CLIENT_USAGE}");
                return None;
            }
        }
    }

    match (addr, name, trace) {
        (Some(addr), Some(name), Some(trace)) => Some(ClientOptions {
            addr,
            name,
            trace,
            neoforge,
            mirror,
            query,
            light_at,
            min_y,
            dump_chunks,
        }),
        _ => {
            eprintln!("{CLIENT_USAGE}");
            None
        }
    }
}

/// `x,y,z`, the form `--light-at` takes.
fn parse_block_position(value: &str) -> Option<(i32, i32, i32)> {
    let mut parts = value.split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    let z = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((x, y, z))
}

async fn run_all_suites(server: &Arc<Server>, addr: SocketAddr) -> Result<Vec<String>, String> {
    let mut summary_lines = Vec::new();
    let mut runner = GameTestRunner::new();

    bot_tests::run_bot_tests(server, addr, &mut runner, &mut summary_lines).await?;
    gametests::run_gametests(server, &mut runner, &mut summary_lines).await?;

    Ok(summary_lines)
}

#[expect(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "CLI test runner outputs results to stdout/stderr"
)]
#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("client") {
        let Some(options) = parse_client_options(&args[1..]) else {
            return ExitCode::FAILURE;
        };
        return match pumpkin_tester::client_trace::run(options).await {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("pumpkin-tester client FAILED: {err}");
                ExitCode::FAILURE
            }
        };
    }

    let _ = rustls::crypto::ring::default_provider().install_default();
    let _ = rayon::ThreadPoolBuilder::new()
        .thread_name(|i| format!("Rayon-Tester-{i}"))
        .build_global();

    let start = Instant::now();
    println!("Starting pumpkin-tester (single server + headless bots + Mojang GameTests)...");

    let temp_dir = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("Failed to create temporary world directory: {err}");
            return ExitCode::FAILURE;
        }
    };

    let server = match harness::create_test_server(&temp_dir).await {
        Ok(s) => s,
        Err(err) => {
            eprintln!("{err}");
            return ExitCode::FAILURE;
        }
    };

    let listener = match TcpListener::bind("127.0.0.1:0").await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("Failed to bind local TCP listener: {err}");
            return ExitCode::FAILURE;
        }
    };
    let local_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(err) => {
            eprintln!("Failed to query local TCP address: {err}");
            return ExitCode::FAILURE;
        }
    };

    let cancel = CancellationToken::new();
    let tasks = TaskTracker::new();
    harness::spawn_java_listener(listener, server.clone(), cancel.clone(), &tasks);

    let result = run_all_suites(&server, local_addr).await;

    cancel.cancel();
    for player in server.get_all_players() {
        player.remove().await;
        server.remove_player(&player);
    }
    tasks.close();
    tasks.wait().await;
    server.shutdown().await;

    match result {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            println!(
                "All bot integration tests and Mojang GameTests passed in {}ms!",
                start.elapsed().as_millis()
            );
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("pumpkin-tester FAILED:\n{err}");
            ExitCode::FAILURE
        }
    }
}
