//! Shutdown handling: on SIGTERM/SIGINT, stop the statement the run is
//! executing instead of waiting for it to finish.
//!
//! The signal handlers set the shutdown flag immediately (so the `run_*` loops
//! stop at their next check) and forward each signal to a watcher task. The
//! watcher opens a short-lived control connection and, depending on
//! `--shutdown-mode`, calls `pg_cancel_backend` / `pg_terminate_backend` on the
//! main session. Only a running VACUUM/ANALYZE is ever acted on, so an idle
//! session (between tables, waiting on replica lag) is left alone and the final
//! logbook and summary work still runs.

use crate::config::CONTROL_CONNECT_TIMEOUT_SECONDS;
use crate::connection;
use crate::logging::{LogLevel, Logger};
use crate::queries;
use crate::types::{ShutdownMode, SslMode};
use anyhow::{Context, Result};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_postgres::Client;

/// Set as soon as the first shutdown signal arrives. Read by `main` to decide
/// the final exit code, since a cancelled/terminated session produces errors
/// that are expected rather than failures.
static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn mark_requested() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn is_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::SeqCst)
}

/// Exit code for a fatal error: 130 if a shutdown was requested (the errors are
/// then a consequence of the cancel/terminate), otherwise the usual mapping.
pub fn exit_code(err: &anyhow::Error, shutdown_requested: bool) -> u8 {
    if shutdown_requested {
        130
    } else {
        crate::db_error::exit_code_for(err)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownAction {
    None,
    Cancel,
    Terminate,
}

/// Decide what to do on the `signal_count`-th signal. `backend_active` says
/// whether there is a statement worth acting on.
pub fn plan_action(mode: ShutdownMode, signal_count: u32, backend_active: bool) -> ShutdownAction {
    if mode == ShutdownMode::Finish || !backend_active {
        return ShutdownAction::None;
    }
    match mode {
        ShutdownMode::Terminate => ShutdownAction::Terminate,
        _ if signal_count >= 2 => ShutdownAction::Terminate,
        _ => ShutdownAction::Cancel,
    }
}

/// Snapshot of the maintenance backend's activity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendState {
    pub active: bool,
    /// Active and running a VACUUM or ANALYZE.
    pub maintenance_running: bool,
    pub query_start: Option<String>,
}

/// Issues cancel/terminate for one backend over its own connection.
pub struct BackendController {
    client: Client,
    pid: i32,
}

impl BackendController {
    pub async fn connect(
        connection_string: &str,
        sslmode: &SslMode,
        ssl_ca_cert: Option<String>,
        ssl_client_cert: Option<String>,
        ssl_client_key: Option<String>,
        pid: i32,
    ) -> Result<Self> {
        let client = tokio::time::timeout(
            Duration::from_secs(CONTROL_CONNECT_TIMEOUT_SECONDS),
            connection::connect_raw(
                connection_string,
                sslmode,
                ssl_ca_cert,
                ssl_client_cert,
                ssl_client_key,
            ),
        )
        .await
        .context("Timed out opening the control connection")??;
        Ok(Self { client, pid })
    }

    pub async fn state(&self) -> Result<BackendState> {
        let row = self
            .client
            .query_opt(queries::GET_BACKEND_ACTIVITY, &[&self.pid])
            .await
            .context("Failed to read backend state")?;
        Ok(match row {
            Some(r) => BackendState {
                active: r.get(0),
                maintenance_running: r.get(1),
                query_start: r.get(2),
            },
            // The backend is already gone.
            None => BackendState {
                active: false,
                maintenance_running: false,
                query_start: None,
            },
        })
    }

    pub async fn cancel(&self) -> Result<bool> {
        let row = self
            .client
            .query_one(queries::CANCEL_BACKEND, &[&self.pid])
            .await
            .context("pg_cancel_backend failed")?;
        Ok(row.get(0))
    }

    pub async fn terminate(&self) -> Result<bool> {
        let row = self
            .client
            .query_one(queries::TERMINATE_BACKEND, &[&self.pid])
            .await
            .context("pg_terminate_backend failed")?;
        Ok(row.get(0))
    }
}

/// Everything the watcher needs to reach the main session.
pub struct WatcherConfig {
    pub mode: ShutdownMode,
    pub grace: Duration,
    pub backend_pid: i32,
    pub connection_string: String,
    pub sslmode: SslMode,
    pub ssl_ca_cert: Option<String>,
    pub ssl_client_cert: Option<String>,
    pub ssl_client_key: Option<String>,
}

/// How long the first signal keeps looking for a running VACUUM/ANALYZE. A
/// signal can land between two statements of the same table (for example
/// between the dead-tuple count and the VACUUM itself).
const POLL_ATTEMPTS: u32 = 8;
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Spawn the watcher. Signals are the names ("SIGTERM"/"SIGINT") sent by the
/// signal handlers; any that arrived before this was spawned are buffered.
pub fn spawn_shutdown_watcher(
    cfg: WatcherConfig,
    signals: UnboundedReceiver<&'static str>,
    logger: Arc<Logger>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(run_watcher(cfg, signals, logger))
}

async fn run_watcher(
    cfg: WatcherConfig,
    mut rx: UnboundedReceiver<&'static str>,
    logger: Arc<Logger>,
) {
    let mut count = 0u32;
    let mut controller: Option<BackendController> = None;

    while let Some(signal) = rx.recv().await {
        count += 1;
        if cfg.mode == ShutdownMode::Finish {
            continue;
        }

        if controller.is_none() {
            match BackendController::connect(
                &cfg.connection_string,
                &cfg.sslmode,
                cfg.ssl_ca_cert.clone(),
                cfg.ssl_client_cert.clone(),
                cfg.ssl_client_key.clone(),
                cfg.backend_pid,
            )
            .await
            {
                Ok(c) => controller = Some(c),
                Err(e) => {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Could not open a control connection to stop the running statement ({e:#}); \
                             falling back to finishing the current table"
                        ),
                    );
                    continue;
                }
            }
        }
        if let Some(c) = controller.as_ref() {
            handle_signal(c, &cfg, &mut rx, &mut count, signal, &logger).await;
        }
    }
}

async fn handle_signal(
    c: &BackendController,
    cfg: &WatcherConfig,
    rx: &mut UnboundedReceiver<&'static str>,
    count: &mut u32,
    signal: &str,
    logger: &Logger,
) {
    let pid = cfg.backend_pid;

    // Find a statement worth acting on. From the second signal on, any active
    // statement counts: the operator wants out.
    let mut target: Option<BackendState> = None;
    for attempt in 0..POLL_ATTEMPTS {
        match c.state().await {
            Ok(s) => {
                let worth_acting = if *count >= 2 {
                    s.active
                } else {
                    s.maintenance_running
                };
                if worth_acting {
                    target = Some(s);
                    break;
                }
            }
            Err(e) => {
                logger.log(
                    LogLevel::Warning,
                    &format!("Could not read the backend state: {e:#}"),
                );
                return;
            }
        }
        if attempt + 1 < POLL_ATTEMPTS {
            tokio::select! {
                _ = tokio::time::sleep(POLL_INTERVAL) => {}
                sig = rx.recv() => {
                    if sig.is_some() {
                        *count += 1;
                    }
                }
            }
        }
    }
    let Some(target) = target else {
        logger.log(
            LogLevel::Info,
            &format!(
                "Shutdown requested ({signal}): no VACUUM/ANALYZE is running — stopping after the current table"
            ),
        );
        return;
    };

    match plan_action(cfg.mode, *count, true) {
        ShutdownAction::None => {}
        ShutdownAction::Terminate => terminate(c, pid, signal, logger).await,
        ShutdownAction::Cancel => {
            let grace = cfg.grace;
            logger.log(
                LogLevel::Warning,
                &format!(
                    "Shutdown requested ({signal}): cancelling running statement on backend PID {pid} via \
                     pg_cancel_backend; will pg_terminate_backend after {}s. Send the signal again to \
                     terminate immediately.",
                    grace.as_secs()
                ),
            );
            if let Err(e) = c.cancel().await {
                logger.log(LogLevel::Warning, &format!("{e:#}"));
            }
            tokio::select! {
                _ = tokio::time::sleep(grace) => {}
                sig = rx.recv() => {
                    if sig.is_some() {
                        *count += 1;
                        terminate(c, pid, signal, logger).await;
                        return;
                    }
                }
            }
            // Escalate only if it is still the same statement.
            match c.state().await {
                Ok(s) if s.active && s.query_start == target.query_start => {
                    logger.log(
                        LogLevel::Warning,
                        &format!(
                            "Statement still running after {}s — terminating backend PID {pid} (pg_terminate_backend).",
                            grace.as_secs()
                        ),
                    );
                    if let Err(e) = c.terminate().await {
                        logger.log(LogLevel::Warning, &format!("{e:#}"));
                    }
                }
                _ => {}
            }
        }
    }
}

async fn terminate(c: &BackendController, pid: i32, signal: &str, logger: &Logger) {
    logger.log(
        LogLevel::Warning,
        &format!(
            "Shutdown requested ({signal}): terminating backend PID {pid} via pg_terminate_backend."
        ),
    );
    if let Err(e) = c.terminate().await {
        logger.log(LogLevel::Warning, &format!("{e:#}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_finish_never_acts() {
        for count in 1..=3 {
            assert_eq!(
                plan_action(ShutdownMode::Finish, count, true),
                ShutdownAction::None
            );
        }
    }

    #[test]
    fn test_cancel_acts_only_on_active_backend() {
        assert_eq!(
            plan_action(ShutdownMode::Cancel, 1, true),
            ShutdownAction::Cancel
        );
        assert_eq!(
            plan_action(ShutdownMode::Cancel, 1, false),
            ShutdownAction::None
        );
    }

    #[test]
    fn test_second_signal_terminates() {
        assert_eq!(
            plan_action(ShutdownMode::Cancel, 2, true),
            ShutdownAction::Terminate
        );
        assert_eq!(
            plan_action(ShutdownMode::Cancel, 2, false),
            ShutdownAction::None
        );
    }

    #[test]
    fn test_terminate_mode() {
        assert_eq!(
            plan_action(ShutdownMode::Terminate, 1, true),
            ShutdownAction::Terminate
        );
        assert_eq!(
            plan_action(ShutdownMode::Terminate, 1, false),
            ShutdownAction::None
        );
    }

    #[test]
    fn test_exit_code_is_130_after_shutdown() {
        let lost = anyhow::Error::new(crate::db_error::ConnectionLost {
            operation: "VACUUM".into(),
            schema: "public".into(),
            table: "t".into(),
            position: None,
            cause: "connection closed".into(),
        });
        assert_eq!(exit_code(&lost, false), 3);
        assert_eq!(exit_code(&lost, true), 130);
        assert_eq!(exit_code(&anyhow::anyhow!("boom"), true), 130);
        assert_eq!(exit_code(&anyhow::anyhow!("boom"), false), 1);
    }

    #[test]
    fn test_shutdown_mode_parsing() {
        assert_eq!("cancel".parse::<ShutdownMode>(), Ok(ShutdownMode::Cancel));
        assert_eq!("FINISH".parse::<ShutdownMode>(), Ok(ShutdownMode::Finish));
        assert_eq!(
            "terminate".parse::<ShutdownMode>(),
            Ok(ShutdownMode::Terminate)
        );
        assert!("kill".parse::<ShutdownMode>().is_err());
        assert_eq!(ShutdownMode::default(), ShutdownMode::Cancel);
    }
}
