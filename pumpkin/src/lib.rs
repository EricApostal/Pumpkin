#![deny(clippy::all)]
#![deny(clippy::pedantic)]
// #![warn(clippy::restriction)]
#![deny(clippy::cargo)]
// to keep consistency
#![deny(clippy::if_then_some_else_none)]
#![deny(clippy::empty_enum_variants_with_brackets)]
#![deny(clippy::empty_structs_with_brackets)]
#![deny(clippy::separated_literal_suffix)]
#![deny(clippy::semicolon_outside_block)]
#![deny(clippy::non_zero_suggestions)]
#![deny(clippy::string_lit_chars_any)]
#![deny(clippy::use_self)]
#![deny(clippy::useless_let_if_seq)]
#![deny(clippy::branches_sharing_code)]
#![deny(clippy::equatable_if_let)]
#![deny(clippy::option_if_let_else)]
// use log crate
#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]
// REMOVE SOME WHEN RELEASE
#![expect(clippy::cargo_common_metadata)]
#![expect(clippy::multiple_crate_versions)]
#![expect(clippy::single_call_fn)]
#![expect(clippy::cast_sign_loss)]
#![expect(clippy::cast_possible_truncation)]
#![expect(clippy::cast_possible_wrap)]
#![expect(clippy::missing_panics_doc)]
#![expect(clippy::missing_errors_doc)]
#![expect(clippy::module_name_repetitions)]
#![expect(clippy::struct_excessive_bools)]

#[cfg(target_os = "wasi")]
compile_error!("Compiling for WASI targets is not supported!");

use log::{Level, LevelFilter, Log, Metadata, Record};
use std::ffi::CString;
use std::io;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::Once;

use net::{lan_broadcast, query, rcon::RCONServer, Client};
use server::{ticker::Ticker, Server};
use tokio::io::{AsyncBufReadExt, BufReader};
#[cfg(not(unix))]
use tokio::signal::ctrl_c;
#[cfg(unix)]
use tokio::signal::unix::{signal, SignalKind};

use std::sync::Arc;

use crate::server::CURRENT_MC_VERSION;
use pumpkin_config::{ADVANCED_CONFIG, BASIC_CONFIG};
use pumpkin_core::text::{color::NamedColor, TextComponent};
use pumpkin_protocol::CURRENT_MC_PROTOCOL;
use std::time::Instant;
use time::macros::format_description;
use time::OffsetDateTime;

static GLOBAL_LOGGER_INIT: Once = Once::new();

type LogCallback = unsafe extern "C" fn(level: i32, message: *const c_char);

static LOG_CALLBACK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

#[repr(C)]
pub struct PumpkinErrorDetails {
    code: i32,
    message: [u8; 256],
    trace: [u8; 1024],
}

#[repr(C)]
pub enum PumpkinError {
    Success = 0,
    LoggerInitializationError = 1,
    NetworkBindError = 2,
    SignalHandlerError = 3,
    RuntimeError = 4,
    ConfigError = 5,
}

struct FFILogger {
    inner_logger: Option<simple_logger::SimpleLogger>,
}

impl FFILogger {
    fn new() -> Self {
        let mut inner_logger = None;

        if ADVANCED_CONFIG.logging.enabled {
            let mut logger = simple_logger::SimpleLogger::new();
            logger = logger.with_timestamp_format(format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]"
            ));

            if !ADVANCED_CONFIG.logging.timestamp {
                logger = logger.without_timestamps();
            }

            if ADVANCED_CONFIG.logging.env {
                logger = logger.env();
            }

            logger = logger.with_level(convert_logger_filter(ADVANCED_CONFIG.logging.level));
            logger = logger.with_colors(ADVANCED_CONFIG.logging.color);
            logger = logger.with_threads(ADVANCED_CONFIG.logging.threads);

            inner_logger = Some(logger);
        }

        Self { inner_logger }
    }
}

impl Log for FFILogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let level = match record.level() {
            Level::Error => 0,
            Level::Warn => 1,
            Level::Info => 2,
            Level::Debug => 3,
            Level::Trace => 4,
        };

        let message = format!("{}", record.args());
        let c_message = match CString::new(message.clone()) {
            Ok(c_str) => c_str,
            Err(_) => return,
        };

        let callback_ptr = LOG_CALLBACK.load(Ordering::Acquire);
        if !callback_ptr.is_null() {
            unsafe {
                let callback: LogCallback = std::mem::transmute(callback_ptr);
                callback(level, c_message.as_ptr());
            }
        }

        if let Some(ref inner_logger) = self.inner_logger {
            let formatted = if ADVANCED_CONFIG.logging.timestamp {
                let now = OffsetDateTime::now_local().unwrap_or(OffsetDateTime::now_utc());
                let format = format_description!("[year]-[month]-[day] [hour]:[minute]:[second]");
                let timestamp = now
                    .format(&format)
                    .unwrap_or_else(|_| "Time error".to_string());
                format!("{} - {}", timestamp, message)
            } else {
                message
            };

            if ADVANCED_CONFIG.logging.color {
                match record.level() {
                    Level::Error => eprintln!("\x1b[31m{}\x1b[0m", formatted),
                    Level::Warn => eprintln!("\x1b[33m{}\x1b[0m", formatted),
                    Level::Info => println!("\x1b[32m{}\x1b[0m", formatted),
                    Level::Debug => println!("\x1b[36m{}\x1b[0m", formatted),
                    Level::Trace => println!("\x1b[90m{}\x1b[0m", formatted),
                }
            } else {
                match record.level() {
                    Level::Error | Level::Warn => eprintln!("{}", formatted),
                    _ => println!("{}", formatted),
                }
            }
        }
    }

    fn flush(&self) {}
}

fn initialize_global_logger() -> Result<(), Box<dyn std::error::Error>> {
    std::thread_local! {
        static INIT_ERROR: std::cell::RefCell<Option<Box<dyn std::error::Error>>> =
            std::cell::RefCell::new(None);
    }

    GLOBAL_LOGGER_INIT.call_once(|| {
        if ADVANCED_CONFIG.logging.enabled {
            let logger = FFILogger::new();
            match log::set_boxed_logger(Box::new(logger)) {
                Ok(()) => {
                    log::set_max_level(LevelFilter::Trace);
                }
                Err(e) => {
                    INIT_ERROR.with(|error| {
                        *error.borrow_mut() = Some(Box::new(e));
                    });
                }
            }
        }
    });

    let error = INIT_ERROR.with(|error| error.borrow_mut().take());
    if let Some(e) = error {
        Err(e)
    } else {
        Ok(())
    }
}

#[no_mangle]
pub extern "C" fn set_log_callback(callback: LogCallback) {
    LOG_CALLBACK.store(callback as *mut (), Ordering::Release);
    if let Err(e) = initialize_global_logger() {
        eprintln!("Failed to initialize logger: {}", e);
    }
}

fn to_pumpkin_error(error: &str) -> PumpkinError {
    match error {
        e if e.contains("logger") => PumpkinError::LoggerInitializationError,
        e if e.contains("bind") => PumpkinError::NetworkBindError,
        e if e.contains("signal") => PumpkinError::SignalHandlerError,
        e if e.contains("config") => PumpkinError::ConfigError,
        _ => PumpkinError::RuntimeError,
    }
}

pub mod block;
pub mod command;
pub mod data;
pub mod entity;
pub mod error;
pub mod net;
pub mod server;
pub mod world;

fn scrub_address(ip: &str) -> String {
    use pumpkin_config::BASIC_CONFIG;
    if BASIC_CONFIG.scrub_ips {
        ip.chars()
            .map(|ch| if ch == '.' || ch == ':' { ch } else { 'x' })
            .collect()
    } else {
        ip.to_string()
    }
}

// Updated to use the consolidated logger initialization
fn init_logger() -> Result<(), Box<dyn std::error::Error>> {
    initialize_global_logger()
}

const fn convert_logger_filter(level: pumpkin_config::logging::LevelFilter) -> LevelFilter {
    match level {
        pumpkin_config::logging::LevelFilter::Off => LevelFilter::Off,
        pumpkin_config::logging::LevelFilter::Error => LevelFilter::Error,
        pumpkin_config::logging::LevelFilter::Warn => LevelFilter::Warn,
        pumpkin_config::logging::LevelFilter::Info => LevelFilter::Info,
        pumpkin_config::logging::LevelFilter::Debug => LevelFilter::Debug,
        pumpkin_config::logging::LevelFilter::Trace => LevelFilter::Trace,
    }
}

const CARGO_PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
const GIT_VERSION: &str = env!("GIT_VERSION");

async fn async_main() -> Result<(), Box<dyn std::error::Error>> {
    let time = Instant::now();

    init_logger()?;

    log::info!("Starting Pumpkin {CARGO_PKG_VERSION} ({GIT_VERSION}) for Minecraft {CURRENT_MC_VERSION} (Protocol {CURRENT_MC_PROTOCOL})");

    log::debug!(
        "Build info: FAMILY: \"{}\", OS: \"{}\", ARCH: \"{}\", BUILD: \"{}\"",
        std::env::consts::FAMILY,
        std::env::consts::OS,
        std::env::consts::ARCH,
        if cfg!(debug_assertions) {
            "Debug"
        } else {
            "Release"
        }
    );

    log::warn!("Pumpkin is currently under heavy development!");
    log::info!("Report Issues on https://github.com/Snowiiii/Pumpkin/issues");
    log::info!("Join our Discord for community support https://discord.com/invite/wT8XjrjKkf");

    tokio::spawn(async {
        setup_sighandler()
            .await
            .map_err(|e| log::error!("Signal handler setup failed: {}", e))
            .ok();
    });

    let listener = tokio::net::TcpListener::bind(BASIC_CONFIG.server_address)
        .await
        .map_err(|e| format!("Failed to bind TCP listener: {}", e))?;

    let addr = listener
        .local_addr()
        .map_err(|e| format!("Unable to get server address: {}", e))?;

    let use_console = ADVANCED_CONFIG.commands.use_console;
    let rcon = ADVANCED_CONFIG.rcon.clone();

    let server = Arc::new(Server::new());
    let mut ticker = Ticker::new(BASIC_CONFIG.tps);

    log::info!("Started Server took {}ms", time.elapsed().as_millis());
    log::info!("You now can connect to the server, Listening on {}", addr);

    if use_console {
        setup_console(server.clone());
    }

    if rcon.enabled {
        let server = server.clone();
        tokio::spawn(async move {
            if let Err(e) = RCONServer::new(&rcon, server).await {
                log::error!("RCON server error: {}", e);
            }
        });
    }

    if ADVANCED_CONFIG.query.enabled {
        log::info!("Query protocol enabled. Starting...");
        tokio::spawn(query::start_query_handler(server.clone(), addr));
    }

    if ADVANCED_CONFIG.lan_broadcast.enabled {
        log::info!("LAN broadcast enabled. Starting...");
        tokio::spawn(lan_broadcast::start_lan_broadcast(addr));
    }

    {
        let server = server.clone();
        tokio::spawn(async move {
            ticker.run(&server).await;
        })
    };

    let mut master_client_id: u16 = 0;
    loop {
        match listener.accept().await {
            Ok((connection, address)) => {
                if let Err(e) = connection.set_nodelay(true) {
                    log::warn!("Failed to set TCP_NODELAY: {}", e);
                }

                let id = master_client_id;
                master_client_id = master_client_id.wrapping_add(1);

                log::info!(
                    "Accepted connection from: {} (id {})",
                    scrub_address(&format!("{address}")),
                    id
                );

                let client = Arc::new(Client::new(connection, addr, id));

                let server = server.clone();
                tokio::spawn(async move {
                    while !client.closed.load(std::sync::atomic::Ordering::Relaxed)
                        && !client
                            .make_player
                            .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        let open = client.poll().await;
                        if open {
                            client.process_packets(&server).await;
                        };
                    }
                    if client
                        .make_player
                        .load(std::sync::atomic::Ordering::Relaxed)
                    {
                        let (player, world) = server.add_player(client).await;
                        world
                            .spawn_player(&BASIC_CONFIG, player.clone(), &server)
                            .await;

                        while !player
                            .client
                            .closed
                            .load(core::sync::atomic::Ordering::Relaxed)
                        {
                            let open = player.client.poll().await;
                            if open {
                                player.process_packets(&server).await;
                            };
                        }
                        log::debug!("Cleaning up player for id {}", id);
                        player.remove().await;
                        server.remove_player().await;
                    }
                });
            }
            Err(e) => {
                log::error!("Error accepting connection: {}", e);
                continue;
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn run_pumpkin() -> PumpkinErrorDetails {
    let mut error_details = PumpkinErrorDetails {
        code: PumpkinError::Success as i32,
        message: [0; 256],
        trace: [0; 1024],
    };
    let runtime_result = std::panic::catch_unwind(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("Failed to create Tokio runtime: {}", e))
            .and_then(|runtime| runtime.block_on(async_main()).map_err(|e| e.to_string()))
    });

    match runtime_result {
        Ok(Ok(_)) => {
            // Success - everything worked
            error_details.code = PumpkinError::Success as i32;
        }
        Ok(Err(error_msg)) => {
            // Runtime error occurred
            log::error!("Runtime error: {}", error_msg);
            let error = to_pumpkin_error(&error_msg);
            error_details.code = error as i32;
            if let Some(msg_bytes) = error_msg.as_bytes().get(..255) {
                error_details.message[..msg_bytes.len()].copy_from_slice(msg_bytes);
            }
        }
        Err(_) => {
            // Panic occurred
            let error_msg = "Panic occurred in Pumpkin server";
            log::error!("{}", error_msg);
            error_details.code = PumpkinError::RuntimeError as i32;
            if let Some(msg_bytes) = error_msg.as_bytes().get(..255) {
                error_details.message[..msg_bytes.len()].copy_from_slice(msg_bytes);
            }
        }
    }

    error_details
}

#[no_mangle]
pub extern "C" fn cleanup_pumpkin() -> PumpkinError {
    PumpkinError::Success
}

fn handle_interrupt() {
    log::warn!(
        "{}",
        TextComponent::text("Received interrupt signal; stopping server...")
            .color_named(NamedColor::Red)
            .to_pretty_console()
    );
    std::process::exit(0);
}

#[cfg(not(unix))]
async fn setup_sighandler() -> io::Result<()> {
    if ctrl_c().await.is_ok() {
        handle_interrupt();
    }
    Ok(())
}

#[cfg(unix)]
async fn setup_sighandler() -> io::Result<()> {
    if signal(SignalKind::interrupt())?.recv().await.is_some() {
        handle_interrupt();
    }

    if signal(SignalKind::hangup())?.recv().await.is_some() {
        handle_interrupt();
    }

    if signal(SignalKind::terminate())?.recv().await.is_some() {
        handle_interrupt();
    }

    Ok(())
}

fn setup_console(server: Arc<Server>) {
    tokio::spawn(async move {
        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        loop {
            let mut out = String::new();

            reader
                .read_line(&mut out)
                .await
                .expect("Failed to read console line");

            if !out.is_empty() {
                let dispatcher = server.command_dispatcher.read().await;
                dispatcher
                    .handle_command(&mut command::CommandSender::Console, &server, &out)
                    .await;
            }
        }
    });
}
