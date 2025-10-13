use std::net::SocketAddr;

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
}
