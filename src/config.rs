use std::net::SocketAddr;

use clap::Parser;

/// Global CLI configuration for Raygun.
#[derive(Debug, Clone, Parser)]
#[command(author, version, about = "Terminal Ray client", long_about = None)]
pub struct Config {
    /// Address Raygun listens on for Ray payloads.
    #[arg(
        long = "bind",
        alias = "bind-addr",
        env = "RAYGUN_BIND",
        value_name = "ADDR",
        default_value = "127.0.0.1:23517",
        help = "Bind address for incoming Ray HTTP requests"
    )]
    pub bind_addr: SocketAddr,
}
