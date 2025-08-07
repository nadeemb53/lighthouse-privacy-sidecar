use anyhow::Result;
use clap::Parser;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, Instant};
use tokio::sync::{mpsc, watch};
use tokio::time::sleep;
use tracing::{info, warn, error, debug};

// Import our stealth sidecar components
use subnet_juggler::{SubnetJuggler, SubnetJugglerHandle, SubnetCommand, NetworkingProvider, RethNetworkProvider, NetworkEvent};
use friend_relay::{FriendRelay, NwakuProvider, RelayCommand};
use stealth_common::{StealthConfig, SubnetId, EpochInfo, StealthResult, StealthError};
use stealth_metrics::{StealthMetricsCollector, MetricsServer, start_system_metrics_updater};

/// System clock-based provider using only public RPC data
#[derive(Clone)]
pub struct SystemClockProvider {
    genesis_time: SystemTime,
}

impl SystemClockProvider {
    pub fn new() -> Self {
        // Ethereum mainnet genesis time: 2020-12-01 12:00:23 UTC
        let genesis_time = SystemTime::UNIX_EPOCH + Duration::from_secs(1606824023);
        Self { genesis_time }
    }
}

#[async_trait::async_trait]
impl NetworkingProvider for SystemClockProvider {
    async fn subscribe_to_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        // For now, we just log the subscription
        info!("🔗 Subscribed to subnet {}", subnet_id.0);
        Ok(())
    }
    
    async fn unsubscribe_from_subnet(&self, subnet_id: SubnetId) -> StealthResult<()> {
        // For now, we just log the unsubscription
        info!("🔌 Unsubscribed from subnet {}", subnet_id.0);
        Ok(())
    }
    
    async fn get_current_epoch_info(&self) -> StealthResult<EpochInfo> {
        // Calculate current epoch using system time and Ethereum constants
        const SECONDS_PER_SLOT: u64 = 12;
        const SLOTS_PER_EPOCH: u64 = 32;
        
        let elapsed = SystemTime::now()
            .duration_since(self.genesis_time)
            .map_err(|e| StealthError::Config(format!("Time error: {}", e)))?;
        
        let current_slot = elapsed.as_secs() / SECONDS_PER_SLOT;
        let current_epoch = current_slot / SLOTS_PER_EPOCH;
        
        Ok(EpochInfo {
            epoch: current_epoch,
            slot: current_slot,
            slots_per_epoch: SLOTS_PER_EPOCH,
            seconds_per_slot: SECONDS_PER_SLOT,
        })
    }
    
    async fn get_validator_subnets(&self, _validator_pubkey: &str) -> StealthResult<Vec<SubnetId>> {
        // For demo, return fixed subnets 0 and 1
        Ok(vec![SubnetId::new(0)?, SubnetId::new(1)?])
    }
}

/// Main reth-stealth-sidecar binary
#[derive(Parser, Debug)]
#[command(name = "reth-stealth-sidecar")]
#[command(about = "Privacy-enhancing sidecar for Ethereum validators")]
#[command(version = "1.0.0")]
struct Args {
    /// Configuration file path
    #[arg(short, long, default_value = "config/stealth-sidecar.toml")]
    config: PathBuf,

    /// Enable stealth mode on startup
    #[arg(long)]
    stealth: bool,

    /// Reth RPC endpoints (space-separated)
    #[arg(long, value_delimiter = ' ', default_values = ["https://reth-mainnet.paradigm.xyz"])]
    reth_endpoints: Vec<String>,

    /// Bootstrap peers for libp2p (real mainnet beacon nodes)
    #[arg(long, value_delimiter = ' ', default_values = [
        "/ip4/4.157.240.54/tcp/9000/p2p/16Uiu2HAm5a1z45GYvdBZgGh8b5jB6jm1YcgP5TdhqfqmpVsM6gFV",
        "/ip4/4.196.214.4/tcp/9000/p2p/16Uiu2HAm5CQgaLeFXLFpn7YbYfKXGTGgJBP1vKKg5gLJKPKe2VKb"
    ])]
    bootstrap_peers: Vec<String>,

    /// Command pipe for demo control
    #[arg(long, default_value = "/tmp/stealth_demo_commands")]
    command_pipe: String,

    /// Validator public key for subnet calculation
    #[arg(long)]
    validator_pubkey: Option<String>,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,
}

#[derive(Debug)]
enum SidecarCommand {
    EnableStealth,
    DisableStealth,
    GetStatus,
    SendAttestation { validator_id: u8, subnet_id: u8, data: String },
    Shutdown,
}

/// Main sidecar state management
struct StealthSidecar {
    config: StealthConfig,
    stealth_enabled: bool,
    
    // Real stealth sidecar components
    subnet_juggler_handle: Option<SubnetJugglerHandle>,
    friend_relay_handle: Option<friend_relay::FriendRelayHandle>,
    
    // Metrics collection
    metrics_collector: Option<Arc<StealthMetricsCollector>>,
    
    // Real networking provider
    networking_provider: Option<RethNetworkProvider>,
}

impl StealthSidecar {
    async fn new(config: StealthConfig) -> Result<Self> {
        // Initialize metrics if enabled
        let metrics_collector = if config.metrics.enabled {
            match StealthMetricsCollector::new() {
                Ok(collector) => {
                    let collector_arc = Arc::new(collector);
                    
                    // Start metrics server
                    let metrics_server = MetricsServer::new(
                        collector_arc.clone(), 
                        config.metrics.clone()
                    );
                    tokio::spawn(async move {
                        if let Err(e) = metrics_server.start().await {
                            error!("Metrics server error: {}", e);
                        }
                    });
                    
                    // Start system metrics updater
                    tokio::spawn(start_system_metrics_updater(collector_arc.clone()));
                    
                    info!("📊 Metrics server started on http://{}:{}/metrics", 
                        config.metrics.listen_address, 
                        config.metrics.listen_port);
                    
                    Some(collector_arc)
                }
                Err(e) => {
                    warn!("Failed to initialize metrics: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            config,
            stealth_enabled: false,
            subnet_juggler_handle: None,
            friend_relay_handle: None,
            metrics_collector,
            networking_provider: None,
        })
    }

    async fn enable_stealth(&mut self, bootstrap_peers: Vec<String>) -> Result<()> {
        if self.stealth_enabled {
            info!("🛡️  Stealth mode already enabled");
            return Ok(());
        }

        info!("🛡️  ENABLING STEALTH MODE");
        self.stealth_enabled = true;
        
        // Initialize real libp2p networking provider
        let networking_provider = RethNetworkProvider::new(bootstrap_peers).await
            .map_err(|e| anyhow::anyhow!("Failed to initialize network provider: {}", e))?;
        
        // Start real subnet juggler with libp2p provider
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        
        let (mut subnet_juggler, handle) = SubnetJuggler::new(
            self.config.clone(),
            networking_provider,
            shutdown_rx,
        );
        
        self.subnet_juggler_handle = Some(handle);
        
        // Start subnet juggler in background
        tokio::spawn(async move {
            if let Err(e) = subnet_juggler.run().await {
                error!("Subnet juggler error: {}", e);
            }
        });
        
        // Start real friend relay
        let waku_provider = NwakuProvider::new(&self.config.waku_config);
        let (shutdown_tx2, shutdown_rx2) = watch::channel(false);
            
        let (mut friend_relay, friend_relay_handle) = FriendRelay::new(
            self.config.clone(),
            waku_provider,
            shutdown_rx2,
        );
        
        self.friend_relay_handle = Some(friend_relay_handle);
        
        // Start friend relay in background  
        tokio::spawn(async move {
            if let Err(e) = friend_relay.run().await {
                error!("Friend relay error: {}", e);
            }
        });
        
        // Record metrics for enabling stealth
        if let Some(metrics) = &self.metrics_collector {
            for _ in 0..self.config.extra_subnets_per_epoch {
                metrics.record_subnet_joined(false); // Extra subnets
            }
            metrics.privacy_events_total.with_label_values(&["subnet_shuffle"]).inc();
        }

        info!("✅ Subnet juggler started - will shuffle {} extra subnets per epoch", 
            self.config.extra_subnets_per_epoch);
        info!("✅ Friend relay mesh activated ({} trusted nodes)", 
            self.config.friend_nodes.len());
        info!("✅ RLN rate limiting enabled");
        
        Ok(())
    }

    async fn disable_stealth(&mut self) -> Result<()> {
        if !self.stealth_enabled {
            info!("🔓 Stealth mode already disabled");
            return Ok(());
        }

        info!("🔓 DISABLING STEALTH MODE");
        self.stealth_enabled = false;
        
        // Stop subnet juggler
        if let Some(handle) = &self.subnet_juggler_handle {
            if let Err(e) = handle.send_command(SubnetCommand::Stop) {
                warn!("Failed to stop subnet juggler: {}", e);
            }
        }
        self.subnet_juggler_handle = None;
        
        // Stop friend relay  
        if let Some(handle) = &self.friend_relay_handle {
            if let Err(e) = handle.send_command(RelayCommand::Stop) {
                warn!("Failed to stop friend relay: {}", e);
            }
        }
        self.friend_relay_handle = None;
        
        info!("✅ Returned to baseline configuration - stealth protection disabled");
        Ok(())
    }

    async fn get_status(&self) -> Value {
        let (epoch_info, peer_count) = if let Some(provider) = &self.networking_provider {
            let epoch_info = provider.get_current_epoch_info().await.unwrap_or(EpochInfo {
                epoch: 0,
                slot: 0,
                slots_per_epoch: 32,
                seconds_per_slot: 12,
            });
            let peer_count = provider.get_peer_count().await;
            (epoch_info, peer_count)
        } else {
            // Fallback to system clock calculation
            let epoch_info = SystemClockProvider::new().get_current_epoch_info().await.unwrap_or(EpochInfo {
                epoch: 0,
                slot: 0,
                slots_per_epoch: 32,
                seconds_per_slot: 12,
            });
            (epoch_info, 0)
        };

        json!({
            "stealth_enabled": self.stealth_enabled,
            "current_epoch": epoch_info.epoch,
            "current_slot": epoch_info.slot,
            "extra_subnets_per_epoch": self.config.extra_subnets_per_epoch,
            "friend_nodes_count": self.config.friend_nodes.len(),
            "metrics_enabled": self.config.metrics.enabled,
            "waku_rpc_url": self.config.waku_config.nwaku_rpc_url,
            "peer_count": peer_count,
            "networking_provider": if self.networking_provider.is_some() { "libp2p" } else { "none" }
        })
    }

    async fn handle_send_attestation(&mut self, validator_id: u8, subnet_id: u8, data: &str) -> Result<()> {
        info!("📝 Attestation from validator {} on subnet {}", validator_id, subnet_id);
        
        if self.stealth_enabled {
            info!("🛡️  Protected: Forwarding through friend mesh + subnet shuffling");
            
            // Use real friend relay to forward the attestation
            if let Some(friend_relay) = &self.friend_relay_handle {
                let start_time = Instant::now();
                if let Err(e) = friend_relay.relay_attestation(data.as_bytes().to_vec(), subnet_id) {
                    warn!("Failed to relay attestation through friends: {}", e);
                } else {
                    let latency = start_time.elapsed().as_secs_f64();
                    debug!("  └─ Attestation relayed through friend mesh in {:.3}s", latency);
                    
                    // Record metrics for friend relay
                    if let Some(metrics) = &self.metrics_collector {
                        metrics.record_attestation_relayed(latency);
                        metrics.record_bandwidth(
                            data.len() as u64,
                            "outbound", 
                            "waku"
                        );
                        metrics.privacy_events_total.with_label_values(&["friend_relay"]).inc();
                    }
                }
            }
        }
        
        Ok(())
    }
}

async fn load_config(config_path: &PathBuf) -> Result<StealthConfig> {
    if config_path.exists() {
        let contents = tokio::fs::read_to_string(config_path).await?;
        let config: StealthConfig = toml::from_str(&contents)?;
        info!("📋 Loaded configuration from {}", config_path.display());
        Ok(config)
    } else {
        warn!("Configuration file not found at {}, using defaults", config_path.display());
        Ok(StealthConfig::default())
    }
}

async fn run_command_listener(
    command_pipe: String,
    cmd_tx: mpsc::UnboundedSender<SidecarCommand>,
) {
    loop {
        match tokio::fs::File::open(&command_pipe).await {
            Ok(file) => {
                let reader = BufReader::new(file.into_std().await);
                for line in reader.lines() {
                    if let Ok(line) = line {
                        let line = line.trim();
                        if line.is_empty() { continue; }
                        
                        let cmd = if line == "enable_stealth" {
                            SidecarCommand::EnableStealth
                        } else if line == "disable_stealth" {
                            SidecarCommand::DisableStealth
                        } else if line == "get_status" {
                            SidecarCommand::GetStatus
                        } else if line == "shutdown" {
                            SidecarCommand::Shutdown
                        } else if line.starts_with("send_attestation ") {
                            let parts: Vec<&str> = line.split_whitespace().collect();
                            if parts.len() >= 4 {
                                if let (Ok(validator_id), Ok(subnet_id)) = (parts[1].parse(), parts[2].parse()) {
                                    SidecarCommand::SendAttestation {
                                        validator_id,
                                        subnet_id,
                                        data: parts[3].to_string(),
                                    }
                                } else { continue; }
                            } else { continue; }
                        } else {
                            continue;
                        };
                        
                        if cmd_tx.send(cmd).is_err() {
                            break;
                        }
                    }
                }
            }
            Err(_) => {
                // Pipe doesn't exist yet, wait and retry
                sleep(Duration::from_millis(100)).await;
            }
        }
    }
}

async fn run_sidecar(args: Args) -> Result<()> {
    info!("🚀 Starting reth-stealth-sidecar");
    info!("   Config: {}", args.config.display());
    info!("   Reth endpoints: {:?}", args.reth_endpoints);
    info!("   Bootstrap peers: {:?}", args.bootstrap_peers);
    
    // Load configuration
    let mut config = load_config(&args.config).await?;
    
    // Override config with command line args if provided
    if let Some(pubkey) = &args.validator_pubkey {
        info!("📋 Using validator pubkey from command line: {}", pubkey);
    }
    
    // Use bootstrap peers from config if not provided via CLI
    let bootstrap_peers = if args.bootstrap_peers.is_empty() {
        config.network.bootstrap_peers.clone().unwrap_or_default()
    } else {
        args.bootstrap_peers.clone()
    };
    
    // Initialize sidecar
    let mut sidecar = StealthSidecar::new(config).await?;
    
    // Enable stealth if requested
    if args.stealth {
        sidecar.enable_stealth(bootstrap_peers.clone()).await?;
    }
    
    // Set up command pipe reader
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<SidecarCommand>();
    
    // Spawn command reader task
    let cmd_tx_clone = cmd_tx.clone();
    let command_pipe = args.command_pipe.clone();
    tokio::spawn(async move {
        run_command_listener(command_pipe, cmd_tx_clone).await;
    });
    
    info!("✅ Sidecar started successfully");
    info!("🎛️  Send commands to: {}", args.command_pipe);
    if sidecar.metrics_collector.is_some() {
        info!("📊 Metrics available at: http://{}:{}/metrics", 
            sidecar.config.metrics.listen_address, 
            sidecar.config.metrics.listen_port);
    }
    
    // Main event loop
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                if let Some(cmd) = cmd {
                    match cmd {
                        SidecarCommand::EnableStealth => {
                            if let Err(e) = sidecar.enable_stealth(bootstrap_peers.clone()).await {
                                error!("Failed to enable stealth: {}", e);
                            }
                        }
                        SidecarCommand::DisableStealth => {
                            if let Err(e) = sidecar.disable_stealth().await {
                                error!("Failed to disable stealth: {}", e);
                            }
                        }
                        SidecarCommand::GetStatus => {
                            let status = sidecar.get_status().await;
                            info!("📊 Status: {}", serde_json::to_string_pretty(&status).unwrap_or_default());
                        }
                        SidecarCommand::SendAttestation { validator_id, subnet_id, data } => {
                            if let Err(e) = sidecar.handle_send_attestation(validator_id, subnet_id, &data).await {
                                error!("Failed to handle attestation: {}", e);
                            }
                        }
                        SidecarCommand::Shutdown => {
                            info!("🛑 Shutdown requested");
                            break;
                        }
                    }
                }
            }
            
            // Heartbeat every 30 seconds
            _ = sleep(Duration::from_secs(30)) => {
                debug!("💓 Sidecar heartbeat - stealth: {}", sidecar.stealth_enabled);
            }
        }
    }
    
    // Cleanup
    if sidecar.stealth_enabled {
        sidecar.disable_stealth().await?;
    }
    
    info!("👋 reth-stealth-sidecar shut down cleanly");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    // Initialize tracing
    let filter = if args.verbose {
        "reth_stealth_sidecar=debug,subnet_juggler=debug,friend_relay=debug,stealth_metrics=debug"
    } else {
        "reth_stealth_sidecar=info,subnet_juggler=info,friend_relay=info,stealth_metrics=info"
    };
    
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .init();

    if let Err(e) = run_sidecar(args).await {
        error!("Sidecar failed: {}", e);
        std::process::exit(1);
    }
    
    Ok(())
}