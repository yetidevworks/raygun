use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;

#[derive(Debug, Clone, Parser)]
pub struct Config {
    /// Address Raygun listens on for Ray payloads.
    #[arg(
        long = "bind",
        alias = "bind-addr",
        env = "RAYGUN_BIND",
        value_name = "ADDR",
        default_value = "0.0.0.0:23517",
        help = "Bind address for incoming Ray HTTP requests"
    )]
    pub bind_addr: SocketAddr,

    /// Optional file path to dump raw Ray payloads for debugging.
    #[arg(
        long = "debug-dump",
        env = "RAYGUN_DEBUG_DUMP",
        value_name = "FILE",
        help = "Append each incoming payload to FILE for offline inspection"
    )]
    pub debug_dump: Option<PathBuf>,
}
