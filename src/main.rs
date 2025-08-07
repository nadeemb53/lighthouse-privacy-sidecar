use anyhow::Result;
use clap::{Arg, Command};
use std::path::PathBuf;
use std::sync::Arc;
use stealth_common::{StealthConfig, StealthError};
use stealth_metrics::StealthMetricsCollector;
use tokio::signal;
use tokio::sync::watch;
use tracing::{error, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod sidecar;
mod reth_integration;

use sidecar::StealthSidecar;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "reth_stealth_sidecar=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting reth-stealth-sidecar v{}", env!("CARGO_PKG_VERSION"));
    info!("A privacy-enhancing sidecar for Ethereum validators");
    info!("Defending against the RAINBOW deanonymization attack");

    // Parse command line arguments
    let matches = Command::new("reth-stealth-sidecar")
        .version(env!("CARGO_PKG_VERSION"))
        .about("Privacy-enhancing sidecar for Ethereum validators to defend against RAINBOW attacks")
        .arg(
            Arg::new("config")
                .short('c')
                .long("config")
                .value_name("FILE")
                .help("Configuration file path")
                .value_parser(clap::value_parser!(PathBuf))
        )
        .arg(
            Arg::new("reth-datadir")
                .long("reth-datadir")
                .value_name("DIR")
                .help("Reth data directory path")
                .value_parser(clap::value_parser!(PathBuf))
        )
        .arg(
            Arg::new("lighthouse-api")
                .long("lighthouse-api")
                .value_name("URL")
                .help("Lighthouse beacon node HTTP API endpoint")
                .default_value("http://localhost:5052")
        )
        .arg(
            Arg::new("extra-subnets")
                .long("extra-subnets")
                .value_name("COUNT")
                .help("Number of extra attestation subnets to join per epoch")
                .default_value("8")
                .value_parser(clap::value_parser!(usize))
        )
        .arg(
            Arg::new("friends")
                .long("friends")
                .value_name("MULTIADDR")
                .help("Friend node multiaddresses for privacy mesh (can be specified multiple times)")
                .action(clap::ArgAction::Append)
        )
        .arg(
            Arg::new("nwaku-rpc")
                .long("nwaku-rpc")
                .value_name("URL")
                .help("nwaku JSON-RPC endpoint for RLN proofs")
                .default_value("http://localhost:8545")
        )
        .arg(
            Arg::new("metrics-port")
                .long("metrics-port")
                .value_name("PORT")
                .help("Prometheus metrics port")
                .default_value("9090")
                .value_parser(clap::value_parser!(u16))
        )
        .arg(
            Arg::new("disable-metrics")
                .long("disable-metrics")
                .help("Disable Prometheus metrics collection")
                .action(clap::ArgAction::SetTrue)
        )
        .get_matches();

    // Load configuration
    let config = if let Some(config_path) = matches.get_one::<PathBuf>("config") {
        load_config_from_file(config_path).await?
    } else {
        build_config_from_args(&matches)?
    };

    // Validate configuration
    stealth_common::utils::validate_config(&config)?;

    info!("Configuration loaded:");
    info!("  Lighthouse API: {}", config.lighthouse_http_api);
    info!("  Extra subnets per epoch: {}", config.extra_subnets_per_epoch);
    info!("  Friend nodes: {}", config.friend_nodes.len());
    info!("  Metrics enabled: {}", config.metrics.enabled);

    // Set up shutdown signal handling
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let shutdown_tx_clone = shutdown_tx.clone();

    tokio::spawn(async move {
        if let Err(e) = signal::ctrl_c().await {
            error!("Failed to listen for ctrl-c: {}", e);
            return;
        }
        info!("Shutdown signal received");
        let _ = shutdown_tx_clone.send(true);
    });

    // Initialize metrics collector
    let metrics_collector = Arc::new(
        StealthMetricsCollector::new()
            .map_err(|e| anyhow::anyhow!("Failed to initialize metrics: {}", e))?
    );

    // Start metrics server if enabled
    let metrics_server_handle = if config.metrics.enabled {
        let server = stealth_metrics::MetricsServer::new(
            metrics_collector.clone(),
            config.metrics.clone(),
        );
        
        let handle = tokio::spawn(async move {
            if let Err(e) = server.start().await {
                error!("Metrics server error: {}", e);
            }
        });
        
        // Start system metrics updater
        let collector_clone = metrics_collector.clone();
        tokio::spawn(stealth_metrics::start_system_metrics_updater(collector_clone));
        
        Some(handle)
    } else {
        None
    };

    // Create and start the stealth sidecar
    let mut sidecar = StealthSidecar::new(config, metrics_collector, shutdown_rx).await?;

    // Run the sidecar
    let result = sidecar.run().await;

    // Clean shutdown
    info!("Shutting down reth-stealth-sidecar...");
    
    // Cancel metrics server
    if let Some(handle) = metrics_server_handle {
        handle.abort();
    }

    match result {
        Ok(_) => {
            info!("reth-stealth-sidecar shutdown complete");
            Ok(())
        }
        Err(e) => {
            error!("reth-stealth-sidecar error: {}", e);
            std::process::exit(1);
        }
    }
}

async fn load_config_from_file(path: &PathBuf) -> Result<StealthConfig> {
    let content = tokio::fs::read_to_string(path).await?;
    let config: StealthConfig = toml::from_str(&content)?;
    Ok(config)
}

fn build_config_from_args(matches: &clap::ArgMatches) -> Result<StealthConfig> {
    use stealth_common::{FriendNodeConfig, MetricsConfig, NetworkConfig, WakuConfig};
    use multiaddr::Multiaddr;

    let mut config = StealthConfig::default();

    // Update from command line arguments
    config.lighthouse_http_api = matches.get_one::<String>("lighthouse-api")
        .unwrap()
        .clone();

    config.extra_subnets_per_epoch = *matches.get_one::<usize>("extra-subnets").unwrap();

    // Parse friend nodes
    if let Some(friend_addrs) = matches.get_many::<String>("friends") {
        config.friend_nodes = friend_addrs
            .enumerate()
            .map(|(i, addr)| {
                let multiaddr: Multiaddr = addr.parse()
                    .map_err(|e| anyhow::anyhow!("Invalid friend multiaddr '{}': {}", addr, e))?;
                
                Ok(FriendNodeConfig {
                    peer_id: format!("friend_{}", i),
                    multiaddr,
                    public_key: format!("pubkey_{}", i), // Placeholder
                })
            })
            .collect::<Result<Vec<_>>>()?;
    }

    config.waku_config.nwaku_rpc_url = matches.get_one::<String>("nwaku-rpc")
        .unwrap()
        .clone();

    config.metrics.enabled = !matches.get_flag("disable-metrics");
    config.metrics.listen_port = *matches.get_one::<u16>("metrics-port").unwrap();

    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[tokio::test]
    async fn test_config_loading() {
        let config_content = r#"
lighthouse_http_api = "http://localhost:5052"
extra_subnets_per_epoch = 6

[waku_config]
nwaku_rpc_url = "http://localhost:8545"
rate_limit_per_epoch = 100

[metrics]
enabled = true
listen_address = "127.0.0.1"
listen_port = 9090

[network]
listen_port = 9000
"#;

        let mut temp_file = NamedTempFile::new().unwrap();
        write!(temp_file, "{}", config_content).unwrap();

        let config = load_config_from_file(&temp_file.path().to_path_buf()).await.unwrap();
        
        assert_eq!(config.lighthouse_http_api, "http://localhost:5052");
        assert_eq!(config.extra_subnets_per_epoch, 6);
        assert_eq!(config.waku_config.nwaku_rpc_url, "http://localhost:8545");
        assert!(config.metrics.enabled);
        assert_eq!(config.metrics.listen_port, 9090);
    }

    #[test]
    fn test_arg_parsing() {
        let args = vec![
            "reth-stealth-sidecar",
            "--lighthouse-api", "http://localhost:5052",
            "--extra-subnets", "10",
            "--friends", "/ip4/127.0.0.1/tcp/60001/p2p/12D3KooWExample1",
            "--friends", "/ip4/127.0.0.1/tcp/60002/p2p/12D3KooWExample2",
            "--nwaku-rpc", "http://localhost:8546",
            "--metrics-port", "9091",
        ];

        let matches = Command::new("test")
            .arg(Arg::new("lighthouse-api").long("lighthouse-api").value_name("URL"))
            .arg(Arg::new("extra-subnets").long("extra-subnets").value_name("COUNT").value_parser(clap::value_parser!(usize)))
            .arg(Arg::new("friends").long("friends").value_name("MULTIADDR").action(clap::ArgAction::Append))
            .arg(Arg::new("nwaku-rpc").long("nwaku-rpc").value_name("URL"))
            .arg(Arg::new("metrics-port").long("metrics-port").value_name("PORT").value_parser(clap::value_parser!(u16)))
            .try_get_matches_from(args)
            .unwrap();

        let config = build_config_from_args(&matches).unwrap();
        
        assert_eq!(config.lighthouse_http_api, "http://localhost:5052");
        assert_eq!(config.extra_subnets_per_epoch, 10);
        assert_eq!(config.friend_nodes.len(), 2);
        assert_eq!(config.waku_config.nwaku_rpc_url, "http://localhost:8546");
        assert_eq!(config.metrics.listen_port, 9091);
    }
}