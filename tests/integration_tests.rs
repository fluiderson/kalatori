// if running locally, ensure that you have no dangling processes (kalatori daemon, chopsticks)
// pkill -f kalatori; pkill -f chopsticks


use kalatori::definitions::api_v2::*;
use std::process::{Command, Child};
use tokio::time::{sleep, Duration};
use reqwest::Client;
use std::env;
use std::sync::{Once, Mutex};
use lazy_static::lazy_static;

static INIT: Once = Once::new();
lazy_static! {
    static ref CHOPSTICKS: Mutex<Option<Child>> = Mutex::new(None);
}

async fn start_chopsticks() -> std::io::Result<Child> {
    let mut command = Command::new("npx");
    command.args(&["@acala-network/chopsticks@latest", "-c", "chopsticks/pd-ah.yml"]);
    let chopsticks = command.spawn()?;
    sleep(Duration::from_secs(3)).await; // Give Chopsticks some time to start
    Ok(chopsticks)
}

async fn stop_chopsticks(chopsticks: &mut Child) -> std::io::Result<()> {
    chopsticks.kill()?;
    chopsticks.wait()?;
    Ok(())
}

const KALATORI_REMARK: &str = "TEST_REMARK";
const KALATORI_CARGO_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

fn load_chain_config() {
    env::set_var("KALATORI_CONFIG", "configs/chopsticks.toml");
    env::set_var("KALATORI_HOST", "127.0.0.1:16726");
    env::set_var("KALATORI_SEED", "bottom drive obey lake curtain smoke basket hold race lonely fit walk");
    env::set_var("KALATORI_RPC", "ws://localhost:8000");
    env::set_var("KALATORI_DECIMALS", "12");
    env::set_var("KALATORI_RECIPIENT", "5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV");
    env::set_var("KALATORI_REMARK", KALATORI_REMARK.to_string());
    // env::set_var("RUST_BACKTRACE", "1");
}

async fn start_daemon() -> std::io::Result<Child> {
    let daemon = Command::new("target/debug/kalatori")
        .spawn()?;
    sleep(Duration::from_secs(3)).await; // Give the daemon some time to start
    Ok(daemon)
}

async fn stop_daemon(daemon: &mut Child) -> std::io::Result<()> {
    daemon.kill()?;
    daemon.wait()?;
    Ok(())
}

struct TestContext {
    daemon: Option<Child>,
}

impl TestContext {
    async fn new() -> Self {
        // Start Chopsticks if not already started
        INIT.call_once(|| {
            tokio::spawn(async {
                let chopsticks = start_chopsticks().await.expect("Failed to start Chopsticks");
                let mut guard = CHOPSTICKS.lock().unwrap();
                *guard = Some(chopsticks);
            });
        });

        // Wait for Chopsticks to start
        sleep(Duration::from_secs(3)).await;

        // Then  start the daemon
        load_chain_config();
        let daemon = start_daemon().await.expect("Failed to start kalatori daemon");

        TestContext {
            daemon: Some(daemon),
        }
    }

    async fn drop_async(&mut self) {
        if let Some(mut daemon) = self.daemon.take() {
            let _ = stop_daemon(&mut daemon).await;
        }
    }
}
#[tokio::test]
async fn test_daemon_status_call() {
    let mut context = TestContext::new().await;

    let client = Client::new();

    let resp = client
        .get("http://127.0.0.1:16726/v2/status")
        .send()
        .await
        .expect("Failed to send request");

    // Assert that the response status is 200 OK
    assert!(resp.status().is_success());

    let body = resp
        .json::<ServerStatus>()
        .await
        .expect("Failed to parse response");

    // Check that all required fields are present
    assert_eq!(body.server_info.version, KALATORI_CARGO_PACKAGE_VERSION);
    assert!(!body.server_info.instance_id.is_empty());
    assert_eq!(body.server_info.debug, true);
    assert_eq!(body.server_info.kalatori_remark, KALATORI_REMARK);

    // Check that supported currencies are present
    // assert!(!body.supported_currencies.is_empty());
    // for (currency, properties) in body.supported_currencies {
    //     assert!(!currency.is_empty());
    //     assert!(!properties.chain_name.is_empty());
    //     assert!(!properties.kind.is_empty());
    //     assert!(properties.decimals > 0);
    //     assert!(!properties.rpc_url.is_empty());
    //     // asset_id is optional, so no need to assert on it
    // }

    context.drop_async().await;
}

// #[tokio::test]
// async fn test_daemon_health_call() {
//     let mut context = TestContext::new().await;
//
//     let client = Client::new();
//
//     let resp = client
//         .get("http://127.0.0.1:16726/v2/health")
//         .send()
//         .await
//         .expect("Failed to send request");
//
//     // Assert that the response status is 200 OK
//     assert!(resp.status().is_success());
//
//     context.drop_async().await;
// }