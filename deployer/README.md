# battleware-deployer

[![Crates.io](https://img.shields.io/crates/v/battleware-deployer.svg)](https://crates.io/crates/battleware-deployer)
[![Docs.rs](https://docs.rs/battleware-deployer/badge.svg)](https://docs.rs/battleware-deployer)

Tools for deploying `battleware`.

## Status

`battleware-deployer` is **ALPHA** software and is not yet recommended for production use. Developers should expect breaking changes and occasional instability.

## Setup

### Local

_To run this example, you must first install [Rust](https://www.rust-lang.org/tools/install)._

#### Create Artifacts

```bash
cargo run --bin battleware-deployer -- generate --peers 5 --bootstrappers 1 --worker-threads 3 --log-level info --message-backlog 16384 --mailbox-size 16384 --deque-size 10 --execution-concurrency 1 --output test --indexer http://127.0.0.1:8080 local --start-port 3000
```

_If setup succeeds, you should see the following output:_

```
2025-09-09T01:03:30.046667Z  INFO battleware_deployer: generated network key identity=b2a5befa67f46b3bf8c8f965de6b3cb5090ac3f6bb3dbc87ff6b55c36f509e87162bbca9c1deb3784723a965c66107d918aead5c2b768e78dd8af0bbd014917d37b9b45c342bfa6754fe27a241f3dcd08015b1fb806d0f734801a1c7e5ec5f07
2025-09-09T01:03:30.047877Z  INFO battleware_deployer: wrote validator configuration file path="4a6bc722a59613d2208284e89d2dd4508dec584f51e8e3cb94415c2cab3b55a8.yaml"
2025-09-09T01:03:30.048151Z  INFO battleware_deployer: wrote validator configuration file path="52af657848395bbab28c6529932c0542a23f0f55d7bfa42af73547b26fe8fcb9.yaml"
2025-09-09T01:03:30.048411Z  INFO battleware_deployer: wrote validator configuration file path="6c186cd9e3ab5c99689c1844d563e568f7d29a22feb2d3424082eb0df62f1581.yaml"
2025-09-09T01:03:30.048659Z  INFO battleware_deployer: wrote validator configuration file path="720f1779931c6850adaaf3f6a4d45c42a4a01ad888c52e27f1c331c2e7b10a2a.yaml"
2025-09-09T01:03:30.048918Z  INFO battleware_deployer: wrote validator configuration file path="be51578d4406281ff85355c3d159cd28ee63addf478ff3147a24514f35f93a3d.yaml"
2025-09-09T01:03:30.049022Z  INFO battleware_deployer: wrote randotron configuration file path="randotron_0.yaml"
2025-09-09T01:03:30.049030Z  INFO battleware_deployer: setup complete bootstrappers=["52af657848395bbab28c6529932c0542a23f0f55d7bfa42af73547b26fe8fcb9"]
To start simulator, run:
cargo run -p battleware-simulator -- --identity b2a5befa67f46b3bf8c8f965de6b3cb5090ac3f6bb3dbc87ff6b55c36f509e87162bbca9c1deb3784723a965c66107d918aead5c2b768e78dd8af0bbd014917d37b9b45c342bfa6754fe27a241f3dcd08015b1fb806d0f734801a1c7e5ec5f07
To start website, run: (in `website` directory)
VITE_IDENTITY=b2a5befa67f46b3bf8c8f965de6b3cb5090ac3f6bb3dbc87ff6b55c36f509e87162bbca9c1deb3784723a965c66107d918aead5c2b768e78dd8af0bbd014917d37b9b45c342bfa6754fe27a241f3dcd08015b1fb806d0f734801a1c7e5ec5f07 VITE_URL=http://127.0.0.1:8080 npm run preview
To start validators, run:
4a6bc722a59613d2208284e89d2dd4508dec584f51e8e3cb94415c2cab3b55a8: cargo run -p battleware-node -- --peers=/Users/patrickogrady/code/battleware/deployer/test/peers.yaml --config=/Users/patrickogrady/code/battleware/deployer/test/4a6bc722a59613d2208284e89d2dd4508dec584f51e8e3cb94415c2cab3b55a8.yaml
52af657848395bbab28c6529932c0542a23f0f55d7bfa42af73547b26fe8fcb9: cargo run -p battleware-node -- --peers=/Users/patrickogrady/code/battleware/deployer/test/peers.yaml --config=/Users/patrickogrady/code/battleware/deployer/test/52af657848395bbab28c6529932c0542a23f0f55d7bfa42af73547b26fe8fcb9.yaml
6c186cd9e3ab5c99689c1844d563e568f7d29a22feb2d3424082eb0df62f1581: cargo run -p battleware-node -- --peers=/Users/patrickogrady/code/battleware/deployer/test/peers.yaml --config=/Users/patrickogrady/code/battleware/deployer/test/6c186cd9e3ab5c99689c1844d563e568f7d29a22feb2d3424082eb0df62f1581.yaml
720f1779931c6850adaaf3f6a4d45c42a4a01ad888c52e27f1c331c2e7b10a2a: cargo run -p battleware-node -- --peers=/Users/patrickogrady/code/battleware/deployer/test/peers.yaml --config=/Users/patrickogrady/code/battleware/deployer/test/720f1779931c6850adaaf3f6a4d45c42a4a01ad888c52e27f1c331c2e7b10a2a.yaml
be51578d4406281ff85355c3d159cd28ee63addf478ff3147a24514f35f93a3d: cargo run -p battleware-node -- --peers=/Users/patrickogrady/code/battleware/deployer/test/peers.yaml --config=/Users/patrickogrady/code/battleware/deployer/test/be51578d4406281ff85355c3d159cd28ee63addf478ff3147a24514f35f93a3d.yaml
To start randotrons, run:
randotron_0: cargo run -p battleware-randotron -- --config=/Users/patrickogrady/code/battleware/deployer/test/randotron_0.yaml
To view metrics, run:
4a6bc722a59613d2208284e89d2dd4508dec584f51e8e3cb94415c2cab3b55a8: curl http://localhost:3001/metrics
52af657848395bbab28c6529932c0542a23f0f55d7bfa42af73547b26fe8fcb9: curl http://localhost:3003/metrics
6c186cd9e3ab5c99689c1844d563e568f7d29a22feb2d3424082eb0df62f1581: curl http://localhost:3005/metrics
720f1779931c6850adaaf3f6a4d45c42a4a01ad888c52e27f1c331c2e7b10a2a: curl http://localhost:3007/metrics
be51578d4406281ff85355c3d159cd28ee63addf478ff3147a24514f35f93a3d: curl http://localhost:3009/metrics
```

#### Start Validators

Run the emitted start commands in separate terminals:

```bash
cargo run --bin validator -- --peers=<your-path>/test/peers.yaml --config=<your-path>/test/10cf8d03daca2332213981adee2a4bfffe4a1782bb5cce036c1d5689c6090997.yaml
```

_It is necessary to start at least one bootstrapper for any other peers to connect (used to exchange IPs to dial, not as a relay)._

#### Debugging

##### Too Many Open Files

If you see an error like `unable to append to journal: Runtime(BlobOpenFailed("engine-consensus", "00000000000000ee", Os { code: 24, kind: Uncategorized, message: "Too many open files" }))`, you may need to increase the maximum number of open files. You can do this by running:

```bash
ulimit -n 65536
```

_MacOS defaults to 256 open files, which is too low for the default settings (where 1 journal file is maintained per recent view)._

### Remote

_To run this example, you must first install [Rust](https://www.rust-lang.org/tools/install) and [Docker](https://www.docker.com/get-started/)._

#### Install `commonware-deployer`

```bash
cargo install commonware-deployer
```

#### Create Artifacts

```bash
cargo run --bin battleware-deployer -- generate --peers 50 --bootstrappers 5 --worker-threads 3 --log-level info --message-backlog 16384 --mailbox-size 16384 --deque-size 10 --execution-concurrency 4 --indexer TODO --output assets remote --regions us-west-1,us-east-1,eu-west-1,ap-northeast-1,eu-north-1,ap-south-1,sa-east-1,eu-central-1,ap-northeast-2,ap-southeast-2 --monitoring-instance-type c7g.4xlarge --monitoring-storage-size 100 --randotron-instance-type c7g.xlarge --randotron-storage-size 25 --instance-type c7g.xlarge --storage-size 25 --dashboard dashboard.json
```

#### Build Validator Binary

##### Build Cross-Platform Compiler

```bash
docker build -t battleware-builder .
```

##### Compile Binary for ARM64

```bash
docker run -it -v ${PWD}/..:/battleware battleware-builder
```

###### Local Compilation

_Before running this command, ensure you change any `version` dependencies you'd like to compile locally to `path` dependencies in `Cargo.toml`._

```bash
docker run -it -v ${PWD}/..:/battleware -v ${PWD}/../../monorepo:/monorepo battleware-builder
```

_Emitted binary `battleware-node` and `battleware-randotron` are placed in `assets`._

#### Deploy Validator Binary

```bash
cd assets
deployer ec2 create --config config.yaml
```

#### Monitor Performance on Grafana

Visit `http://<monitoring-ip>:3000/d/chain`

_This dashboard is only accessible from the IP used to deploy the infrastructure._

#### [Optional] Update Validator Binary

##### Re-Compile Binary for ARM64

```bash
docker run -it -v ${PWD}/..:/battleware battleware-builder
```

##### Restart Validator Binary on EC2 Instances

```bash
deployer ec2 update --config config.yaml
```

#### Destroy Infrastructure

```bash
deployer ec2 destroy --config config.yaml
```

#### Debugging

##### Missing AWS Credentials

If `commonware-deployer` can't detect your AWS credentials, you'll see a "Request has expired." error:

```
2025-03-05T01:36:47.550105Z  INFO deployer::ec2::create: created EC2 client region="eu-west-1"
2025-03-05T01:36:48.268330Z ERROR deployer: failed to create EC2 deployment error=AwsEc2(Unhandled(Unhandled { source: ErrorMetadata { code: Some("RequestExpired"), message: Some("Request has expired."), extras: Some({"aws_request_id": "006f6b92-4965-470d-8eac-7c9644744bdf"}) }, meta: ErrorMetadata { code: Some("RequestExpired"), message: Some("Request has expired."), extras: Some({"aws_request_id": "006f6b92-4965-470d-8eac-7c9644744bdf"}) } }))
```

##### EC2 Throttling

EC2 instances may throttle network traffic if a workload exceeds the allocation for a particular instance type. To check
if an instance is throttled, SSH into the instance and run:

```bash
ethtool -S ens5 | grep "allowance"
```

If throttled, you'll see a non-zero value for some "allowance" item:

```txt
bw_in_allowance_exceeded: 0
bw_out_allowance_exceeded: 14368
pps_allowance_exceeded: 0
conntrack_allowance_exceeded: 0
linklocal_allowance_exceeded: 0
```