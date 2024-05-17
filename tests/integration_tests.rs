use std::process::{Command, Child};
use tokio::time::{sleep, Duration};
use reqwest::Client;
use serde_json::json;
use std::env;
use std::io::Result;

fn load_chain_config() {
    env::set_var("KALATORI_CONFIG", "configs/chopsticks.toml");
    env::set_var("KALATORI_HOST", "127.0.0.1:16726");
    env::set_var("KALATORI_SEED", "bottom drive obey lake curtain smoke basket hold race lonely fit walk");
    env::set_var("KALATORI_RPC", "wss://westend-rpc.polkadot.io");
    env::set_var("KALATORI_DECIMALS", "12");
    env::set_var("KALATORI_RECIPIENT", "5DfhGyQdFobKM8NsWvEeAKk5EQQgYe9AydgJ7rMB6E1EqRzV");
    env::set_var("KALATORI_REMARK", "KALATORI_REMARK");
    // env::set_var("RUST_BACKTRACE", "1");
}

async fn start_daemon() -> Result<Child> {
    let daemon = Command::new("target/debug/kalatori")
        .spawn()?;
    // Give the daemon some time to start
    sleep(Duration::from_secs(5)).await;
    Ok(daemon)
}

async fn stop_daemon(daemon: &mut Child) -> Result<()> {
    daemon.kill()?;
    daemon.wait()?;
    Ok(())
}

#[tokio::test]
async fn test_daemon_health_check() {
    // Load chain configuration
    load_chain_config();

    // Start the daemon
    let mut daemon = start_daemon().await.expect("Failed to start kalatori daemon");

    // Create an HTTP client
    let client = Client::new();

    // Send a health check request
    let resp = client
        .get("http://127.0.0.1:16726/v2/status")
        .send()
        .await
        .expect("Failed to send request");

    // Assert that the response status is 200 OK
    assert!(resp.status().is_success());

    // Shutdown the daemon
    stop_daemon(&mut daemon).await.expect("Failed to kill kalatori daemon");
}
