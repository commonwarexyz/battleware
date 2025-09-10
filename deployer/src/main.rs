use battleware_node::{Config as ValidatorConfig, Peers};
use battleware_randotron::Config as RandotronConfig;
use clap::{value_parser, Arg, ArgMatches, Command};
use commonware_codec::Encode;
use commonware_cryptography::{
    bls12381::{
        dkg::ops,
        primitives::{poly, variant::MinSig},
    },
    ed25519::PrivateKey,
    PrivateKeyExt, Signer,
};
use commonware_deployer::ec2::{self, METRICS_PORT};
use commonware_utils::{hex, quorum};
use rand::{rngs::OsRng, seq::IteratorRandom, RngCore};
use std::{
    collections::HashMap,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tracing::{error, info};
use uuid::Uuid;

const VALIDATOR_PACKAGE: &str = "battleware-node";
const RANDOTRON_PACKAGE: &str = "battleware-randotron";
const PORT: u16 = 4545;
const STORAGE_CLASS: &str = "gp3";
const DASHBOARD_FILE: &str = "dashboard.json";
const RANDOTRON_SEED_LENGTH: usize = 32;

fn main() {
    // Initialize logger
    tracing_subscriber::fmt().init();

    // Define the main command with subcommands
    let app = Command::new("setup")
        .about("Manage configuration files for an battleware deployment.")
        .subcommand(
            Command::new("generate")
                .about("Generate configuration files for an battleware deployment")
                .arg(
                    Arg::new("peers")
                        .long("peers")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("bootstrappers")
                        .long("bootstrappers")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("worker_threads")
                        .long("worker-threads")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("execution_concurrency")
                        .long("execution-concurrency")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("log_level")
                        .long("log-level")
                        .required(true)
                        .value_parser(value_parser!(String)),
                )
                .arg(
                    Arg::new("message_backlog")
                        .long("message-backlog")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("mailbox_size")
                        .long("mailbox-size")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("deque_size")
                        .long("deque-size")
                        .required(true)
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("output")
                        .long("output")
                        .required(true)
                        .value_parser(value_parser!(String)),
                )
                .arg(
                    Arg::new("indexer")
                        .long("indexer")
                        .required(true)
                        .value_parser(value_parser!(String)),
                )
                .arg(
                    Arg::new("randotron_keys")
                        .long("randotron-keys")
                        .default_value("11")
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("randotron_instances")
                        .long("randotron-instances")
                        .default_value("1")
                        .value_parser(value_parser!(usize)),
                )
                .arg(
                    Arg::new("randotron_worker_threads")
                        .long("randotron-worker-threads")
                        .default_value("4")
                        .value_parser(value_parser!(usize)),
                )
                .subcommand(Command::new("local").about("Generate configuration files for local deployment")
                    .arg(
                        Arg::new("start_port")
                            .long("start-port")
                            .required(true)
                            .value_parser(value_parser!(u16)),
                    )
            )
                .subcommand(
                    Command::new("remote")
                        .about("Generate configuration files for `commonware-deployer`-managed deployment")
                        .arg(
                            Arg::new("regions")
                                .long("regions")
                                .required(true)
                                .value_delimiter(',')
                                .value_parser(value_parser!(String)),
                        )
                        .arg(
                            Arg::new("instance_type")
                                .long("instance-type")
                                .required(true)
                                .value_parser(value_parser!(String)),
                        )
                        .arg(
                            Arg::new("storage_size")
                                .long("storage-size")
                                .required(true)
                                .value_parser(value_parser!(i32)),
                        )
                        .arg(
                            Arg::new("randotron_instance_type")
                                .long("randotron-instance-type")
                                .default_value("c7g.xlarge")
                                .value_parser(value_parser!(String)),
                        )
                        .arg(
                            Arg::new("randotron_storage_size")
                                .long("randotron-storage-size")
                                .default_value("25")
                                .value_parser(value_parser!(i32)),
                        )
                        .arg(
                            Arg::new("monitoring_instance_type")
                                .long("monitoring-instance-type")
                                .required(true)
                                .value_parser(value_parser!(String)),
                        )
                        .arg(
                            Arg::new("monitoring_storage_size")
                                .long("monitoring-storage-size")
                                .required(true)
                                .value_parser(value_parser!(i32)),
                        )
                        .arg(
                            Arg::new("dashboard")
                                .long("dashboard")
                                .required(true)
                                .value_parser(value_parser!(String)),
                        ),
                ),
        );

    // Parse arguments
    let matches = app.get_matches();

    // Handle subcommands
    match matches.subcommand() {
        Some(("generate", sub_matches)) => {
            let peers = *sub_matches.get_one::<usize>("peers").unwrap();
            let bootstrappers = *sub_matches.get_one::<usize>("bootstrappers").unwrap();
            let worker_threads = *sub_matches.get_one::<usize>("worker_threads").unwrap();
            let log_level = sub_matches.get_one::<String>("log_level").unwrap().clone();
            let message_backlog = *sub_matches.get_one::<usize>("message_backlog").unwrap();
            let mailbox_size = *sub_matches.get_one::<usize>("mailbox_size").unwrap();
            let deque_size = *sub_matches.get_one::<usize>("deque_size").unwrap();
            let output = sub_matches.get_one::<String>("output").unwrap().clone();
            let indexer = sub_matches.get_one::<String>("indexer").unwrap().clone();
            assert!(indexer.to_uppercase() != "TODO", "indexer cannot be 'TODO'");
            let execution_concurrency = *sub_matches
                .get_one::<usize>("execution_concurrency")
                .unwrap();
            let randotron_keys = *sub_matches.get_one::<usize>("randotron_keys").unwrap();
            let randotron_instances = *sub_matches.get_one::<usize>("randotron_instances").unwrap();
            let randotron_worker_threads = *sub_matches
                .get_one::<usize>("randotron_worker_threads")
                .unwrap();
            match sub_matches.subcommand() {
                Some(("local", sub_matches)) => generate_local(
                    sub_matches,
                    peers,
                    bootstrappers,
                    worker_threads,
                    log_level,
                    message_backlog,
                    mailbox_size,
                    deque_size,
                    output,
                    indexer,
                    execution_concurrency,
                    randotron_keys,
                    randotron_instances,
                    randotron_worker_threads,
                ),
                Some(("remote", sub_matches)) => generate_remote(
                    sub_matches,
                    peers,
                    bootstrappers,
                    worker_threads,
                    log_level,
                    message_backlog,
                    mailbox_size,
                    deque_size,
                    output,
                    indexer,
                    execution_concurrency,
                    randotron_keys,
                    randotron_instances,
                    randotron_worker_threads,
                ),
                _ => {
                    eprintln!("Invalid subcommand. Use 'local' or 'remote'.");
                    std::process::exit(1);
                }
            }
        }
        _ => {
            eprintln!("Invalid subcommand. Use 'generate'.");
            std::process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_local(
    sub_matches: &ArgMatches,
    peers: usize,
    bootstrappers: usize,
    worker_threads: usize,
    log_level: String,
    message_backlog: usize,
    mailbox_size: usize,
    deque_size: usize,
    output: String,
    indexer: String,
    execution_concurrency: usize,
    randotron_keys: usize,
    randotron_instances: usize,
    randotron_worker_threads: usize,
) {
    // Extract arguments
    let start_port = *sub_matches.get_one::<u16>("start_port").unwrap();

    // Construct output path
    let raw_current_dir = std::env::current_dir().unwrap();
    let current_dir = raw_current_dir.to_str().unwrap();
    let output = format!("{current_dir}/{output}");
    let storage_output = format!("{output}/storage");

    // Check if output directory exists
    if fs::metadata(&output).is_ok() {
        error!("output directory already exists: {}", output);
        std::process::exit(1);
    }

    // Generate peers
    assert!(
        bootstrappers <= peers,
        "bootstrappers must be less than or equal to peers"
    );
    let mut peer_signers = (0..peers)
        .map(|_| PrivateKey::from_rng(&mut OsRng))
        .collect::<Vec<_>>();
    peer_signers.sort_by_key(|signer| signer.public_key());
    let allowed_peers: Vec<String> = peer_signers
        .iter()
        .map(|signer| signer.public_key().to_string())
        .collect();
    let bootstrappers = allowed_peers
        .iter()
        .choose_multiple(&mut OsRng, bootstrappers)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();

    // Generate consensus key
    let peers_u32 = peers as u32;
    let threshold = quorum(peers_u32);
    let (polynomial, shares) =
        ops::generate_shares::<_, MinSig>(&mut OsRng, None, peers_u32, threshold);
    let identity = poly::public::<MinSig>(&polynomial);
    info!(?identity, "generated network key");

    // Generate validator instance configurations
    let mut port = start_port;
    let mut addresses = HashMap::new();
    let mut validator_configurations = Vec::new();
    for (signer, share) in peer_signers.iter().zip(shares.iter()) {
        // Create peer config
        let name = signer.public_key().to_string();
        addresses.insert(
            name.clone(),
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port),
        );
        let peer_config_file = format!("{name}.yaml");
        let directory = format!("{storage_output}/{name}");
        let peer_config = ValidatorConfig {
            private_key: signer.to_string(),
            share: hex(&share.encode()),
            polynomial: hex(&polynomial.encode()),

            port,
            metrics_port: port + 1,
            directory,
            worker_threads,
            log_level: log_level.clone(),

            allowed_peers: allowed_peers.clone(),
            bootstrappers: bootstrappers.clone(),

            message_backlog,
            mailbox_size,
            deque_size,

            indexer: indexer.clone(),
            execution_concurrency,
        };
        validator_configurations.push((name, peer_config_file.clone(), peer_config));
        port += 2;
    }

    // Generate randotron configurations
    let mut randotron_configurations = Vec::new();
    for i in 0..randotron_instances {
        let mut seed = [0u8; RANDOTRON_SEED_LENGTH];
        OsRng.fill_bytes(&mut seed);

        let randotron_config_file = format!("randotron_{i}.yaml");
        let randotron_config = RandotronConfig {
            num_keys: randotron_keys,
            base_url: indexer.clone(),
            network_identity: hex(&identity.encode()),
            log_level: log_level.clone(),
            seed: hex(&seed),
            worker_threads: randotron_worker_threads,
        };
        randotron_configurations.push((
            format!("randotron_{i}"),
            randotron_config_file,
            randotron_config,
        ));
    }

    // Create required output directories
    fs::create_dir_all(&output).unwrap();
    fs::create_dir_all(&storage_output).unwrap();

    // Write peers file
    let peers_path = format!("{output}/peers.yaml");
    let file = fs::File::create(&peers_path).unwrap();
    serde_yaml::to_writer(file, &Peers { addresses }).unwrap();

    // Write validator configuration files
    for (_, peer_config_file, peer_config) in &validator_configurations {
        let path = format!("{output}/{peer_config_file}");
        let file = fs::File::create(&path).unwrap();
        serde_yaml::to_writer(file, peer_config).unwrap();
        info!(
            path = peer_config_file,
            "wrote validator configuration file"
        );
    }

    // Write randotron configuration files
    for (_, randotron_config_file, randotron_config) in &randotron_configurations {
        let path = format!("{output}/{randotron_config_file}");
        let file = fs::File::create(&path).unwrap();
        serde_yaml::to_writer(file, randotron_config).unwrap();
        info!(
            path = randotron_config_file,
            "wrote randotron configuration file"
        );
    }

    // Emit start commands
    info!(?bootstrappers, "setup complete");
    println!("To start simulator, run:");
    println!("cargo run -p battleware-simulator -- --identity {identity}");
    println!("To start website, run: (in `website` directory)");
    println!("VITE_IDENTITY={identity} VITE_URL={indexer} npm run preview");
    println!("To start validators, run:");
    for (name, peer_config_file, _) in &validator_configurations {
        let path = format!("{output}/{peer_config_file}");
        let command =
            format!("cargo run -p {VALIDATOR_PACKAGE} -- --peers={peers_path} --config={path}");
        println!("{name}: {command}");
    }
    println!("To start randotrons, run:");
    for (name, randotron_config_file, _) in &randotron_configurations {
        let path = format!("{output}/{randotron_config_file}");
        let command = format!("cargo run -p {RANDOTRON_PACKAGE} -- --config={path}");
        println!("{name}: {command}");
    }
    println!("To view metrics, run:");
    for (name, _, peer_config) in validator_configurations {
        println!(
            "{}: curl http://localhost:{}/metrics",
            name, peer_config.metrics_port
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn generate_remote(
    sub_matches: &ArgMatches,
    peers: usize,
    bootstrappers: usize,
    worker_threads: usize,
    log_level: String,
    message_backlog: usize,
    mailbox_size: usize,
    deque_size: usize,
    output: String,
    indexer: String,
    execution_concurrency: usize,
    randotron_keys: usize,
    randotron_instances: usize,
    randotron_worker_threads: usize,
) {
    // Extract arguments
    let regions = sub_matches
        .get_many::<String>("regions")
        .unwrap()
        .cloned()
        .collect::<Vec<_>>();
    let instance_type = sub_matches
        .get_one::<String>("instance_type")
        .unwrap()
        .clone();
    let storage_size = *sub_matches.get_one::<i32>("storage_size").unwrap();
    let randotron_instance_type = sub_matches
        .get_one::<String>("randotron_instance_type")
        .unwrap()
        .clone();
    let randotron_storage_size = *sub_matches
        .get_one::<i32>("randotron_storage_size")
        .unwrap();
    let monitoring_instance_type = sub_matches
        .get_one::<String>("monitoring_instance_type")
        .unwrap()
        .clone();
    let monitoring_storage_size = *sub_matches
        .get_one::<i32>("monitoring_storage_size")
        .unwrap();
    let dashboard = sub_matches.get_one::<String>("dashboard").unwrap().clone();

    // Construct output path
    let raw_current_dir = std::env::current_dir().unwrap();
    let current_dir = raw_current_dir.to_str().unwrap();
    let output = format!("{current_dir}/{output}");

    // Check if output directory exists
    if fs::metadata(&output).is_ok() {
        error!("output directory already exists: {}", output);
        std::process::exit(1);
    }

    // Generate UUID
    let tag = Uuid::new_v4().to_string();
    info!(tag, "generated deployment tag");

    // Generate peers
    assert!(
        bootstrappers <= peers,
        "bootstrappers must be less than or equal to peers"
    );
    let mut peer_signers = (0..peers)
        .map(|_| PrivateKey::from_rng(&mut OsRng))
        .collect::<Vec<_>>();
    peer_signers.sort_by_key(|signer| signer.public_key());
    let allowed_peers: Vec<String> = peer_signers
        .iter()
        .map(|signer| signer.public_key().to_string())
        .collect();
    let bootstrappers = allowed_peers
        .iter()
        .choose_multiple(&mut OsRng, bootstrappers)
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();

    // Generate consensus key
    let peers_u32 = peers as u32;
    let threshold = quorum(peers_u32);
    let (polynomial, shares) =
        ops::generate_shares::<_, MinSig>(&mut OsRng, None, peers_u32, threshold);
    let identity = poly::public::<MinSig>(&polynomial);
    info!(?identity, "generated network key");

    // Generate validator instance configurations
    assert!(
        regions.len() <= peers,
        "must be at least one peer per specified region"
    );
    let mut instance_configs = Vec::new();
    let mut validator_configs = Vec::new();
    for (index, signer) in peer_signers.iter().enumerate() {
        // Create peer config
        let name = signer.public_key().to_string();
        let peer_config_file = format!("{name}.yaml");
        let peer_config = ValidatorConfig {
            private_key: signer.to_string(),
            share: hex(&shares[index].encode()),
            polynomial: hex(&polynomial.encode()),

            port: PORT,
            metrics_port: METRICS_PORT,
            directory: "/home/ubuntu/data".to_string(),
            worker_threads,
            log_level: log_level.clone(),

            allowed_peers: allowed_peers.clone(),
            bootstrappers: bootstrappers.clone(),

            message_backlog,
            mailbox_size,
            deque_size,

            indexer: indexer.clone(),
            execution_concurrency,
        };
        validator_configs.push((peer_config_file.clone(), peer_config));

        // Create instance config
        let region_index = index % regions.len();
        let region = regions[region_index].clone();
        let instance = ec2::InstanceConfig {
            name: name.clone(),
            region,
            instance_type: instance_type.clone(),
            storage_size,
            storage_class: STORAGE_CLASS.to_string(),
            binary: VALIDATOR_PACKAGE.to_string(),
            config: peer_config_file,
            profiling: false,
        };
        instance_configs.push(instance);
    }

    // Generate randotron instance configurations
    let mut randotron_configs = Vec::new();
    for i in 0..randotron_instances {
        let mut seed = [0u8; RANDOTRON_SEED_LENGTH];
        OsRng.fill_bytes(&mut seed);

        let randotron_name = format!("randotron_{i}");
        let randotron_config_file = format!("{randotron_name}.yaml");
        let randotron_config = RandotronConfig {
            num_keys: randotron_keys,
            base_url: indexer.clone(),
            network_identity: hex(&identity.encode()),
            log_level: log_level.clone(),
            seed: hex(&seed),
            worker_threads: randotron_worker_threads,
        };
        randotron_configs.push((randotron_config_file.clone(), randotron_config));

        // Create instance config for randotron
        let region_index = i % regions.len();
        let region = regions[region_index].clone();
        let instance = ec2::InstanceConfig {
            name: randotron_name,
            region,
            instance_type: randotron_instance_type.clone(),
            storage_size: randotron_storage_size,
            storage_class: STORAGE_CLASS.to_string(),
            binary: RANDOTRON_PACKAGE.to_string(),
            config: randotron_config_file,
            profiling: false,
        };
        instance_configs.push(instance);
    }

    // Generate root config file
    let config = ec2::Config {
        tag,
        instances: instance_configs,
        monitoring: ec2::MonitoringConfig {
            instance_type: monitoring_instance_type,
            storage_size: monitoring_storage_size,
            storage_class: STORAGE_CLASS.to_string(),
            dashboard: DASHBOARD_FILE.to_string(),
        },
        ports: vec![ec2::PortConfig {
            protocol: "tcp".to_string(),
            port: PORT,
            cidr: "0.0.0.0/0".to_string(),
        }],
    };

    // Write configuration files
    fs::create_dir_all(&output).unwrap();
    // Dashboard should be in the deployer directory
    let dashboard_path = if dashboard.starts_with('/') {
        dashboard.clone()
    } else {
        // Check if we're in the deployer directory or the root directory
        let candidate_paths = vec![
            format!("{current_dir}/{dashboard}"),
            format!("{current_dir}/deployer/{dashboard}"),
        ];
        candidate_paths
            .into_iter()
            .find(|p| fs::metadata(p).is_ok())
            .unwrap_or_else(|| {
                error!("Could not find dashboard file: {}", dashboard);
                std::process::exit(1);
            })
    };
    fs::copy(dashboard_path, format!("{output}/{DASHBOARD_FILE}")).unwrap();

    // Write validator configs
    for (peer_config_file, peer_config) in validator_configs {
        let path = format!("{output}/{peer_config_file}");
        let file = fs::File::create(&path).unwrap();
        serde_yaml::to_writer(file, &peer_config).unwrap();
        info!(
            path = peer_config_file,
            "wrote validator configuration file"
        );
    }

    // Write randotron configs
    for (randotron_config_file, randotron_config) in randotron_configs {
        let path = format!("{output}/{randotron_config_file}");
        let file = fs::File::create(&path).unwrap();
        serde_yaml::to_writer(file, &randotron_config).unwrap();
        info!(
            path = randotron_config_file,
            "wrote randotron configuration file"
        );
    }

    // Write main config
    let path = format!("{output}/config.yaml");
    let file = fs::File::create(&path).unwrap();
    serde_yaml::to_writer(file, &config).unwrap();
    info!(path = "config.yaml", "wrote configuration file");
}
