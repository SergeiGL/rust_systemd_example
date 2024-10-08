/*

cargo build --release && sudo cp target/release/vpn_linux /usr/local/bin/ && sudo chmod +x /usr/local/bin/vpn_linux && sudo cp vpn-service.service /lib/systemd/system/ && sudo systemctl daemon-reload

*/

use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use systemd::daemon;
use tokio::fs;
use tokio::signal::unix::{signal, SignalKind};
use tokio::time::{sleep, timeout, Duration};

const PATH_TO_CONF: &str = "/etc/vpn.conf";
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
const WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);


#[tokio::main]
async fn main() -> Result<()> {
   
    let (is_running, _config, tasks) = start_service().await?;

    // Main loop
    while is_running.load(Ordering::Relaxed) {
        sleep(Duration::from_secs(1)).await;
    }

    graceful_shutdown(tasks).await?;
    Ok(())
}




async fn start_service() -> Result<(Arc<AtomicBool>, Arc<String>, Vec<tokio::task::JoinHandle<Result<(), anyhow::Error>>>)>{
    println!("Starting...");

    let config = Arc::new(read_config(PATH_TO_CONF).await?);
    let is_running = Arc::new(AtomicBool::new(true));

    daemon::notify(false, [(daemon::STATE_READY, "1")].iter())
        .context("Failed to notify systemd of ready state")?;

    let tasks = spawn_tasks(is_running.clone(), config.clone());
    Ok((is_running, config, tasks))
}

fn spawn_tasks(is_running: Arc<AtomicBool>, config: Arc<String>) -> Vec<tokio::task::JoinHandle<Result<()>>> {
    vec![
        tokio::spawn(handle_signals(is_running.clone())),
        tokio::spawn(handle_config_reload(is_running.clone(), config)),
        tokio::spawn(watchdog_handle(is_running)),
    ]
}

async fn graceful_shutdown(tasks: Vec<tokio::task::JoinHandle<Result<()>>>) -> Result<()> {
    println!("Shutting down...");
    
    let shutdown_result = timeout(SHUTDOWN_TIMEOUT, async {
        for task in tasks {
            if let Err(e) = task.await {
                eprintln!("Task error during shutdown: {:?}", e);
            }
        }
    }).await;

    match shutdown_result {
        Ok(_) => println!("All tasks shut down successfully"),
        Err(_) => eprintln!("Shutdown timed out after {:?}", SHUTDOWN_TIMEOUT),
    }

    daemon::notify(false, [(daemon::STATE_STOPPING, "1")].iter())
        .context("Failed to notify systemd of stopping state")?;
    
    Ok(())
}

async fn watchdog_handle(is_running: Arc<AtomicBool>) -> Result<()> {
    while is_running.load(Ordering::Relaxed) {
        sleep(WATCHDOG_INTERVAL).await;
        daemon::notify(false, [(daemon::STATE_WATCHDOG, "1")].iter())
            .context("Failed to send watchdog notification")?;
    }
    Ok(())
}


async fn handle_signals(is_running: Arc<AtomicBool>) -> Result<()> {
    let mut term_signal = signal(SignalKind::terminate())?;
    let mut int_signal = signal(SignalKind::interrupt())?;

    tokio::select! {
        _ = term_signal.recv() => println!("Received SIGTERM"),
        _ = int_signal.recv() => println!("Received SIGINT"),
    }

    is_running.store(false, Ordering::Relaxed);
    Ok(())
}

async fn read_config(path: &str) -> Result<String> {
    fs::read_to_string(path)
        .await
        .with_context(|| format!("Failed to read config file: {}", path))
}

async fn handle_config_reload(is_running: Arc<AtomicBool>, mut config: Arc<String>) -> Result<()> {
    let mut hup_signal = signal(SignalKind::hangup())?;

    while is_running.load(Ordering::Relaxed) {
        tokio::select! {
            _ = hup_signal.recv() => {
                println!("Received SIGHUP, reloading config");
                match read_config(PATH_TO_CONF).await {
                    Ok(new_config) => {
                        *Arc::make_mut(&mut config) = new_config;
                        println!("Config reloaded successfully");
                    }
                    Err(e) => eprintln!("Failed to reload config: {}", e),
                }
            }
            _ = sleep(Duration::from_secs(1)) => {
                if !is_running.load(Ordering::Relaxed) {
                    break;
                }
            }
        }
    }

    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;
    use tokio::fs::File;
    use tokio::io::AsyncWriteExt;

    
    #[tokio::test]
    async fn read_config_success() {
        let tmp_file_path = "/tmp/test_vpn.conf";
        let config_content = "1.23/54dzp';:*()$@^%~ 23323\nport = 2323\n1\n2";
        
        let mut file = File::create(tmp_file_path).await.unwrap();
        file.write_all(config_content.as_bytes()).await.unwrap();

        let result = read_config(tmp_file_path).await;
        
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), config_content);

        let _ = tokio::fs::remove_file(tmp_file_path).await;
    }

    #[tokio::test]
    async fn read_config_file_not_found() {
        let non_existing_path = "/tmp/non_existent_vpn.conf";
        
        let result = read_config(non_existing_path).await;

        assert!(result.is_err());
    }
}
