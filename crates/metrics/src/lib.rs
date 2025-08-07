use chrono::{DateTime, Utc};
use prometheus::{
    Counter, CounterVec, Gauge, GaugeVec, Histogram, HistogramOpts, HistogramVec, IntCounter,
    IntCounterVec, IntGauge, Opts, Registry,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use stealth_common::{MetricsConfig, StealthError, StealthResult};
use tokio::sync::RwLock;
use tracing::{error, info, warn};
use warp::Filter;

/// Custom metrics collector for the stealth sidecar
pub struct StealthMetricsCollector {
    registry: Registry,
    
    // Subnet juggler metrics
    pub subnets_joined_total: IntCounterVec,
    pub subnets_left_total: IntCounterVec,
    pub current_subscribed_subnets: IntGauge,
    pub epoch_reshuffle_duration: Histogram,
    pub lighthouse_api_requests_total: IntCounterVec,
    pub lighthouse_api_request_duration: HistogramVec,
    
    // Friend relay metrics
    pub attestations_relayed_total: IntCounter,
    pub attestations_received_total: IntCounter,
    pub friend_relay_latency: Histogram,
    pub friend_messages_sent_total: IntCounterVec,
    pub friend_messages_received_total: IntCounterVec,
    pub rln_proofs_generated_total: IntCounter,
    pub rln_proofs_verified_total: IntCounterVec,
    pub rate_limit_violations_total: IntCounter,
    
    // Network metrics
    pub bandwidth_bytes_total: CounterVec,
    pub peer_connections: GaugeVec,
    pub message_size_bytes: HistogramVec,
    
    // System metrics
    pub uptime_seconds: Gauge,
    pub memory_usage_bytes: Gauge,
    pub cpu_usage_percent: Gauge,
    
    // Attack defense metrics
    pub rainbow_attack_attempts_detected: IntCounter,
    pub privacy_events_total: IntCounterVec,
    
    start_time: DateTime<Utc>,
}

impl StealthMetricsCollector {
    pub fn new() -> StealthResult<Self> {
        let registry = Registry::new();
        
        // Subnet juggler metrics
        let subnets_joined_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_subnets_joined_total",
                "Total number of attestation subnets joined"
            ),
            &["subnet_type"] // "mandatory" or "extra"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create subnets_joined_total: {}", e)))?;
        
        let subnets_left_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_subnets_left_total",
                "Total number of attestation subnets left"
            ),
            &["subnet_type"]
        ).map_err(|e| StealthError::Metrics(format!("Failed to create subnets_left_total: {}", e)))?;
        
        let current_subscribed_subnets = IntGauge::new(
            "stealth_sidecar_current_subscribed_subnets",
            "Current number of subscribed attestation subnets"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create current_subscribed_subnets: {}", e)))?;
        
        let epoch_reshuffle_duration = Histogram::with_opts(
            HistogramOpts::new(
                "stealth_sidecar_epoch_reshuffle_duration_seconds",
                "Time taken to reshuffle subnets at epoch boundary"
            )
        ).map_err(|e| StealthError::Metrics(format!("Failed to create epoch_reshuffle_duration: {}", e)))?;
        
        let lighthouse_api_requests_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_lighthouse_api_requests_total",
                "Total number of Lighthouse API requests"
            ),
            &["endpoint", "status"] // endpoint path and HTTP status
        ).map_err(|e| StealthError::Metrics(format!("Failed to create lighthouse_api_requests_total: {}", e)))?;
        
        let lighthouse_api_request_duration = HistogramVec::new(
            HistogramOpts::new(
                "stealth_sidecar_lighthouse_api_request_duration_seconds",
                "Duration of Lighthouse API requests"
            ),
            &["endpoint"]
        ).map_err(|e| StealthError::Metrics(format!("Failed to create lighthouse_api_request_duration: {}", e)))?;
        
        // Friend relay metrics
        let attestations_relayed_total = IntCounter::new(
            "stealth_sidecar_attestations_relayed_total",
            "Total number of attestations relayed through friends"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create attestations_relayed_total: {}", e)))?;
        
        let attestations_received_total = IntCounter::new(
            "stealth_sidecar_attestations_received_total",
            "Total number of attestations received from friends"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create attestations_received_total: {}", e)))?;
        
        let friend_relay_latency = Histogram::with_opts(
            HistogramOpts::new(
                "stealth_sidecar_friend_relay_latency_seconds",
                "End-to-end latency for relaying attestations through friends"
            )
        ).map_err(|e| StealthError::Metrics(format!("Failed to create friend_relay_latency: {}", e)))?;
        
        let friend_messages_sent_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_friend_messages_sent_total",
                "Total number of messages sent to friend nodes"
            ),
            &["friend_id", "status"] // friend peer_id and success/failure
        ).map_err(|e| StealthError::Metrics(format!("Failed to create friend_messages_sent_total: {}", e)))?;
        
        let friend_messages_received_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_friend_messages_received_total",
                "Total number of messages received from friend nodes"
            ),
            &["friend_id"]
        ).map_err(|e| StealthError::Metrics(format!("Failed to create friend_messages_received_total: {}", e)))?;
        
        let rln_proofs_generated_total = IntCounter::new(
            "stealth_sidecar_rln_proofs_generated_total",
            "Total number of RLN proofs generated"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create rln_proofs_generated_total: {}", e)))?;
        
        let rln_proofs_verified_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_rln_proofs_verified_total",
                "Total number of RLN proofs verified"
            ),
            &["result"] // "valid" or "invalid"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create rln_proofs_verified_total: {}", e)))?;
        
        let rate_limit_violations_total = IntCounter::new(
            "stealth_sidecar_rate_limit_violations_total",
            "Total number of RLN rate limit violations detected"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create rate_limit_violations_total: {}", e)))?;
        
        // Network metrics
        let bandwidth_bytes_total = CounterVec::new(
            Opts::new(
                "stealth_sidecar_bandwidth_bytes_total",
                "Total bandwidth usage in bytes"
            ),
            &["direction", "protocol"] // "inbound"/"outbound", "lighthouse"/"waku"/"gossip"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create bandwidth_bytes_total: {}", e)))?;
        
        let peer_connections = GaugeVec::new(
            Opts::new(
                "stealth_sidecar_peer_connections",
                "Number of active peer connections"
            ),
            &["peer_type"] // "friend", "lighthouse", "gossip"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create peer_connections: {}", e)))?;
        
        let message_size_bytes = HistogramVec::new(
            HistogramOpts::new(
                "stealth_sidecar_message_size_bytes",
                "Size distribution of messages processed"
            ),
            &["message_type"] // "attestation", "rln_proof", "gossip"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create message_size_bytes: {}", e)))?;
        
        // System metrics
        let uptime_seconds = Gauge::new(
            "stealth_sidecar_uptime_seconds",
            "Uptime of the stealth sidecar in seconds"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create uptime_seconds: {}", e)))?;
        
        let memory_usage_bytes = Gauge::new(
            "stealth_sidecar_memory_usage_bytes",
            "Memory usage of the stealth sidecar process"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create memory_usage_bytes: {}", e)))?;
        
        let cpu_usage_percent = Gauge::new(
            "stealth_sidecar_cpu_usage_percent",
            "CPU usage percentage of the stealth sidecar process"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create cpu_usage_percent: {}", e)))?;
        
        // Attack defense metrics
        let rainbow_attack_attempts_detected = IntCounter::new(
            "stealth_sidecar_rainbow_attack_attempts_detected_total",
            "Total number of potential RAINBOW attack attempts detected"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create rainbow_attack_attempts_detected: {}", e)))?;
        
        let privacy_events_total = IntCounterVec::new(
            Opts::new(
                "stealth_sidecar_privacy_events_total",
                "Total number of privacy-related events"
            ),
            &["event_type"] // "subnet_shuffle", "friend_relay", "anonymity_preserved"
        ).map_err(|e| StealthError::Metrics(format!("Failed to create privacy_events_total: {}", e)))?;
        
        // Register all metrics
        registry.register(Box::new(subnets_joined_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(subnets_left_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(current_subscribed_subnets.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(epoch_reshuffle_duration.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(lighthouse_api_requests_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(lighthouse_api_request_duration.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(attestations_relayed_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(attestations_received_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(friend_relay_latency.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(friend_messages_sent_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(friend_messages_received_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(rln_proofs_generated_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(rln_proofs_verified_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(rate_limit_violations_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(bandwidth_bytes_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(peer_connections.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(message_size_bytes.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(uptime_seconds.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(memory_usage_bytes.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(cpu_usage_percent.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(rainbow_attack_attempts_detected.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        registry.register(Box::new(privacy_events_total.clone())).map_err(|e| StealthError::Metrics(format!("Registry error: {}", e)))?;
        
        Ok(Self {
            registry,
            subnets_joined_total,
            subnets_left_total,
            current_subscribed_subnets,
            epoch_reshuffle_duration,
            lighthouse_api_requests_total,
            lighthouse_api_request_duration,
            attestations_relayed_total,
            attestations_received_total,
            friend_relay_latency,
            friend_messages_sent_total,
            friend_messages_received_total,
            rln_proofs_generated_total,
            rln_proofs_verified_total,
            rate_limit_violations_total,
            bandwidth_bytes_total,
            peer_connections,
            message_size_bytes,
            uptime_seconds,
            memory_usage_bytes,
            cpu_usage_percent,
            rainbow_attack_attempts_detected,
            privacy_events_total,
            start_time: Utc::now(),
        })
    }
    
    /// Update system metrics
    pub fn update_system_metrics(&self) {
        // Update uptime
        let uptime = Utc::now().signed_duration_since(self.start_time);
        self.uptime_seconds.set(uptime.num_seconds() as f64);
        
        // In a real implementation, these would read from /proc or use system APIs
        // For demo purposes, we'll use placeholder values
        self.memory_usage_bytes.set(128_000_000.0); // 128MB
        self.cpu_usage_percent.set(15.5); // 15.5%
    }
    
    /// Record subnet join operation
    pub fn record_subnet_joined(&self, is_mandatory: bool) {
        let subnet_type = if is_mandatory { "mandatory" } else { "extra" };
        self.subnets_joined_total.with_label_values(&[subnet_type]).inc();
        self.privacy_events_total.with_label_values(&["subnet_shuffle"]).inc();
    }
    
    /// Record subnet leave operation
    pub fn record_subnet_left(&self, is_mandatory: bool) {
        let subnet_type = if is_mandatory { "mandatory" } else { "extra" };
        self.subnets_left_total.with_label_values(&[subnet_type]).inc();
    }
    
    /// Record epoch reshuffle timing
    pub fn record_epoch_reshuffle(&self, duration_seconds: f64) {
        self.epoch_reshuffle_duration.observe(duration_seconds);
        self.privacy_events_total.with_label_values(&["subnet_shuffle"]).inc();
    }
    
    /// Record Lighthouse API request
    pub fn record_lighthouse_api_request(&self, endpoint: &str, status: &str, duration_seconds: f64) {
        self.lighthouse_api_requests_total.with_label_values(&[endpoint, status]).inc();
        self.lighthouse_api_request_duration.with_label_values(&[endpoint]).observe(duration_seconds);
    }
    
    /// Record attestation relay
    pub fn record_attestation_relayed(&self, latency_seconds: f64) {
        self.attestations_relayed_total.inc();
        self.friend_relay_latency.observe(latency_seconds);
        self.privacy_events_total.with_label_values(&["friend_relay"]).inc();
    }
    
    /// Record friend message
    pub fn record_friend_message_sent(&self, friend_id: &str, success: bool) {
        let status = if success { "success" } else { "failure" };
        self.friend_messages_sent_total.with_label_values(&[friend_id, status]).inc();
    }
    
    /// Record RLN proof generation
    pub fn record_rln_proof_generated(&self) {
        self.rln_proofs_generated_total.inc();
    }
    
    /// Record RLN proof verification
    pub fn record_rln_proof_verified(&self, valid: bool) {
        let result = if valid { "valid" } else { "invalid" };
        self.rln_proofs_verified_total.with_label_values(&[result]).inc();
    }
    
    /// Record rate limit violation
    pub fn record_rate_limit_violation(&self) {
        self.rate_limit_violations_total.inc();
    }
    
    /// Record bandwidth usage
    pub fn record_bandwidth(&self, bytes: u64, direction: &str, protocol: &str) {
        self.bandwidth_bytes_total.with_label_values(&[direction, protocol]).inc_by(bytes as f64);
    }
    
    /// Record message size
    pub fn record_message_size(&self, bytes: usize, message_type: &str) {
        self.message_size_bytes.with_label_values(&[message_type]).observe(bytes as f64);
    }
    
    /// Update peer connection count
    pub fn update_peer_connections(&self, peer_type: &str, count: i64) {
        self.peer_connections.with_label_values(&[peer_type]).set(count as f64);
    }
    
    /// Record potential attack detection
    pub fn record_rainbow_attack_detected(&self) {
        self.rainbow_attack_attempts_detected.inc();
        self.privacy_events_total.with_label_values(&["anonymity_preserved"]).inc();
    }
    
    /// Get the Prometheus registry for HTTP endpoint
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

/// HTTP server for exposing Prometheus metrics
pub struct MetricsServer {
    collector: Arc<StealthMetricsCollector>,
    config: MetricsConfig,
}

impl MetricsServer {
    pub fn new(collector: Arc<StealthMetricsCollector>, config: MetricsConfig) -> Self {
        Self { collector, config }
    }
    
    /// Start the HTTP metrics server
    pub async fn start(&self) -> StealthResult<()> {
        if !self.config.enabled {
            info!("Metrics collection disabled");
            return Ok(());
        }
        
        let collector = self.collector.clone();
        
        let metrics_route = warp::path("metrics")
            .and(warp::get())
            .map(move || {
                let encoder = prometheus::TextEncoder::new();
                let metric_families = collector.registry().gather();
                match encoder.encode_to_string(&metric_families) {
                    Ok(output) => {
                        warp::http::Response::builder()
                            .header("content-type", "text/plain; version=0.0.4")
                            .body(output)
                    }
                    Err(e) => {
                        error!("Failed to encode metrics: {}", e);
                        warp::http::Response::builder()
                            .status(500)
                            .body("Internal Server Error".to_string())
                    }
                }
            });
        
        let health_route = warp::path("health")
            .and(warp::get())
            .map(|| {
                warp::reply::json(&serde_json::json!({
                    "status": "healthy",
                    "timestamp": Utc::now().to_rfc3339(),
                    "service": "reth-stealth-sidecar"
                }))
            });
        
        let routes = metrics_route.or(health_route);
        
        let addr: SocketAddr = format!("{}:{}", self.config.listen_address, self.config.listen_port)
            .parse()
            .map_err(|e| StealthError::Metrics(format!("Invalid listen address: {}", e)))?;
        
        info!("Starting metrics server on http://{}/metrics", addr);
        
        warp::serve(routes).run(addr).await;
        
        Ok(())
    }
}

/// Periodic task to update system metrics
pub async fn start_system_metrics_updater(collector: Arc<StealthMetricsCollector>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(10));
    
    loop {
        interval.tick().await;
        collector.update_system_metrics();
    }
}

/// Summary statistics for dashboard display
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardStats {
    pub uptime_seconds: f64,
    pub subnets_subscribed: i64,
    pub attestations_relayed: u64,
    pub friends_connected: i64,
    pub average_latency_ms: f64,
    pub bandwidth_mbps: f64,
    pub memory_usage_mb: f64,
    pub cpu_usage_percent: f64,
    pub privacy_events_count: u64,
    pub last_updated: DateTime<Utc>,
}

impl DashboardStats {
    pub fn from_collector(collector: &StealthMetricsCollector) -> Self {
        // In a real implementation, these would query the actual metric values
        // For demo purposes, we'll use placeholder calculations
        
        Self {
            uptime_seconds: collector.uptime_seconds.get(),
            subnets_subscribed: collector.current_subscribed_subnets.get(),
            attestations_relayed: collector.attestations_relayed_total.get(),
            friends_connected: 3, // Would come from peer_connections metric
            average_latency_ms: 36.0, // Would come from friend_relay_latency metric
            bandwidth_mbps: 0.8, // Would be calculated from bandwidth_bytes_total
            memory_usage_mb: collector.memory_usage_bytes.get() / 1_000_000.0,
            cpu_usage_percent: collector.cpu_usage_percent.get(),
            privacy_events_count: 0, // Would come from privacy_events_total
            last_updated: Utc::now(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_metrics_collector_creation() {
        let collector = StealthMetricsCollector::new().unwrap();
        
        // Test that metrics can be updated
        collector.record_subnet_joined(false);
        collector.record_attestation_relayed(0.036);
        collector.record_rln_proof_generated();
        
        // Check that counters have been incremented
        assert_eq!(collector.subnets_joined_total.with_label_values(&["extra"]).get(), 1);
        assert_eq!(collector.attestations_relayed_total.get(), 1);
        assert_eq!(collector.rln_proofs_generated_total.get(), 1);
    }
    
    #[test]
    fn test_dashboard_stats() {
        let collector = StealthMetricsCollector::new().unwrap();
        collector.record_subnet_joined(true);
        collector.record_subnet_joined(false);
        
        let stats = DashboardStats::from_collector(&collector);
        
        assert!(stats.uptime_seconds >= 0.0);
        assert_eq!(stats.attestations_relayed, 0);
        assert!(stats.last_updated <= Utc::now());
    }
    
    #[tokio::test]
    async fn test_metrics_server_disabled() {
        let collector = Arc::new(StealthMetricsCollector::new().unwrap());
        let config = MetricsConfig {
            enabled: false,
            listen_address: "127.0.0.1".to_string(),
            listen_port: 0,
        };
        
        let server = MetricsServer::new(collector, config);
        
        // Should return Ok when disabled
        assert!(server.start().await.is_ok());
    }
}