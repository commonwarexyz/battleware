use battleware_client::Client;
use battleware_randotron::{Config, Engine, EngineConfig};
use clap::{Arg, Command};
use commonware_runtime::{tokio, Metrics, Runner};
use std::str::FromStr;
use tracing::{info, Level};

fn main() {
    // Parse arguments
    let matches = Command::new("randotron")
        .about("Randomly battle players on a battleware chain.")
        .arg(Arg::new("hosts").long("hosts").required(false)) // needed for compatibility with commonware-deployer
        .arg(Arg::new("config").long("config").required(true))
        .get_matches();

    // Load from config file
    let hosts_file = matches.get_one::<String>("hosts");
    let config_file = matches.get_one::<String>("config").unwrap();
    let config_file = std::fs::read_to_string(config_file).expect("Could not read config file");
    let config: Config = serde_yaml::from_str(&config_file).expect("Could not parse config file");

    // Initialize runtime
    let cfg = tokio::Config::default()
        .with_tcp_nodelay(Some(true))
        .with_worker_threads(config.worker_threads)
        .with_catch_panics(true);
    let executor = tokio::Runner::new(cfg);

    // Start runtime
    executor.start(|context| async move {
        // Setup logging
        let level = Level::from_str(&config.log_level).expect("Invalid log level");
        tokio::telemetry::init(
            context.with_label("telemetry"),
            tokio::telemetry::Logging {
                level,
                // If we are using `commonware-deployer`, we should use structured logging.
                json: hosts_file.is_some(),
            },
            None, // no metrics
            None, // no dashboard
        );
        info!(
            num_keys = config.num_keys,
            base_url = config.base_url,
            seed = config.seed,
            "Starting randotron"
        );

        // Decode network identity
        let network_identity_bytes = commonware_utils::from_hex(&config.network_identity)
            .expect("Failed to decode network identity hex");
        let network_identity =
            commonware_codec::DecodeExt::decode(&mut &network_identity_bytes[..])
                .expect("Failed to decode network identity");
        let seed_bytes =
            commonware_utils::from_hex(&config.seed).expect("Failed to decode seed hex");
        let seed = seed_bytes
            .try_into()
            .expect("Seed must be exactly 32 bytes");

        // Start engine
        let client = Client::new(&config.base_url, network_identity);
        let engine = Engine::new(
            context,
            EngineConfig {
                num_keys: config.num_keys,
                network_identity,
                seed,
            },
            client,
        )
        .await
        .expect("Failed to create engine");
        engine.run().await;
    });
}
