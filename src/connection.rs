use crate::credentials::get_password_from_pgpass;
use crate::types::SslMode;
use anyhow::{Context, Result};
use native_tls::{Certificate, Identity, TlsConnector};
use postgres_native_tls::MakeTlsConnector;
use std::{env, fs};
use tokio_postgres::{Config, NoTls, config::SslMode as TokioSslMode};
use zeroize::Zeroizing;

use crate::config::{
    DEFAULT_POSTGRES_DATABASE, DEFAULT_POSTGRES_HOST, DEFAULT_POSTGRES_PORT,
    DEFAULT_POSTGRES_USERNAME,
};

/// Password wrapper that zeroes memory on drop and hides the value from Debug output.
pub struct SecretString(Zeroizing<String>);

impl SecretString {
    pub fn new(s: String) -> Self {
        Self(Zeroizing::new(s))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl Clone for SecretString {
    fn clone(&self) -> Self {
        Self::new(self.0.as_str().to_owned())
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[REDACTED]")
    }
}

/// All parameters needed to open a PostgreSQL connection.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    pub host: String,
    pub port: u16,
    pub database: String,
    pub username: String,
    pub password: Option<SecretString>,
    pub sslmode: SslMode,
    pub ssl_ca_cert: Option<String>,
    pub ssl_client_cert: Option<String>,
    pub ssl_client_key: Option<String>,
    pub connect_timeout_seconds: u64,
}

impl ConnectionConfig {
    /// Build from CLI args / env-vars / .pgpass, in that precedence order.
    #[allow(clippy::too_many_arguments)]
    pub fn from_args(
        host: Option<String>,
        port: Option<u16>,
        database: Option<String>,
        username: Option<String>,
        password: Option<String>,
        sslmode: SslMode,
        ssl_ca_cert: Option<String>,
        ssl_client_cert: Option<String>,
        ssl_client_key: Option<String>,
        connect_timeout_seconds: u64,
    ) -> Result<Self> {
        let host = host
            .or_else(|| env::var("PG_HOST").ok())
            .unwrap_or_else(|| DEFAULT_POSTGRES_HOST.to_string());

        let port = port
            .or_else(|| env::var("PG_PORT").ok().and_then(|p| p.parse().ok()))
            .unwrap_or(DEFAULT_POSTGRES_PORT);

        let database = database
            .or_else(|| env::var("PG_DATABASE").ok())
            .unwrap_or_else(|| DEFAULT_POSTGRES_DATABASE.to_string());

        let username = username
            .or_else(|| env::var("PG_USER").ok())
            .unwrap_or_else(|| DEFAULT_POSTGRES_USERNAME.to_string());

        let password = password
            .or_else(|| {
                let env_pw = env::var("PG_PASSWORD").ok();
                // treat empty string same as missing
                if env_pw.as_ref().is_some_and(|p| p.is_empty()) {
                    None
                } else {
                    env_pw
                }
            })
            .or_else(|| {
                // Read password from file path specified in PG_PASSWORD_FILE
                env::var("PG_PASSWORD_FILE").ok().and_then(|path| {
                    fs::read_to_string(&path).ok().map(|content| {
                        // Strip trailing newline that secret files may have
                        content.trim_end().to_string()
                    })
                })
            })
            .or_else(|| get_password_from_pgpass(&host, port, &database, &username).unwrap_or(None))
            .map(SecretString::new);

        Ok(Self {
            host,
            port,
            database,
            username,
            password,
            sslmode,
            ssl_ca_cert,
            ssl_client_cert,
            ssl_client_key,
            connect_timeout_seconds,
        })
    }

    pub fn build_connection_string(&self) -> String {
        let mut s = format!(
            "host={} port={} dbname={} user={} connect_timeout={}",
            escape_libpq_value(&self.host),
            self.port,
            escape_libpq_value(&self.database),
            escape_libpq_value(&self.username),
            self.connect_timeout_seconds,
        );
        if let Some(ref pw) = self.password {
            s.push_str(&format!(" password={}", escape_libpq_value(pw.expose())));
        }
        s
    }
}

fn escape_libpq_value(s: &str) -> String {
    if s.chars()
        .any(|c| c.is_whitespace() || c == '\'' || c == '\\')
    {
        let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{escaped}'")
    } else {
        s.to_owned()
    }
}

/// Returns the server's numeric version (server_version_num), e.g. 160003 for 16.3.
pub async fn get_server_version_num(client: &tokio_postgres::Client) -> Result<i32> {
    let row = client
        .query_one(crate::queries::GET_SERVER_VERSION_NUM, &[])
        .await
        .context("Failed to read server_version_num")?;
    Ok(row.get(0))
}

/// Formats a server_version_num for display, e.g. 160003 -> "16.3", 90605 -> "9.6.5".
fn format_pg_version(version_num: i32) -> String {
    if version_num >= 100000 {
        format!("{}.{}", version_num / 10000, version_num % 10000)
    } else {
        format!(
            "{}.{}.{}",
            version_num / 10000,
            (version_num / 100) % 100,
            version_num % 100
        )
    }
}

/// Open a connection with optional SSL, then set basic session parameters.
pub async fn connect(
    connection_string: &str,
    sslmode: &SslMode,
    ssl_ca_cert: Option<String>,
    ssl_client_cert: Option<String>,
    ssl_client_key: Option<String>,
    statement_timeout_seconds: u64,
) -> Result<tokio_postgres::Client> {
    let client = connect_raw(
        connection_string,
        sslmode,
        ssl_ca_cert,
        ssl_client_cert,
        ssl_client_key,
    )
    .await?;

    // Check minimum PostgreSQL version before setting session parameters
    let version_num = get_server_version_num(&client).await?;
    if version_num < crate::config::MIN_SUPPORTED_PG_VERSION_NUM {
        return Err(anyhow::anyhow!(
            "pg-maintainer requires {}+ — connected server reports {}. \
             Session settings this tool depends on (e.g. idle_session_timeout) \
             were introduced in PostgreSQL 14; older servers are not supported.",
            crate::config::MIN_SUPPORTED_PG_VERSION_LABEL,
            format_pg_version(version_num),
        ));
    }

    let statement_timeout_sql = if statement_timeout_seconds == 0 {
        "SET statement_timeout TO 0".to_string()
    } else {
        format!("SET statement_timeout TO '{statement_timeout_seconds}s'")
    };
    client
        .execute(&statement_timeout_sql, &[])
        .await
        .context("Failed to set statement_timeout")?;
    client
        .execute(crate::queries::SET_IDLE_SESSION_TIMEOUT, &[])
        .await
        .context("Failed to set idle_session_timeout")?;
    client
        .execute(crate::queries::SET_APPLICATION_NAME, &[])
        .await
        .context("Failed to set application_name")?;

    Ok(client)
}

/// Open a raw connection (no session parameters set).
pub async fn connect_raw(
    connection_string: &str,
    sslmode: &SslMode,
    ssl_ca_cert: Option<String>,
    ssl_client_cert: Option<String>,
    ssl_client_key: Option<String>,
) -> Result<tokio_postgres::Client> {
    if *sslmode == SslMode::Disable {
        let (client, conn) = tokio_postgres::connect(connection_string, NoTls)
            .await
            .context("Failed to connect to PostgreSQL")?;
        tokio::spawn(async move {
            if let Err(e) = conn.await {
                eprintln!("Connection error: {e}");
            }
        });
        return Ok(client);
    }

    // SSL path
    let mut cfg: Config = connection_string
        .parse()
        .context("Failed to parse connection string")?;
    cfg.ssl_mode(TokioSslMode::Require);

    let mut tls_builder = TlsConnector::builder();
    match sslmode {
        SslMode::Require => {
            tls_builder.danger_accept_invalid_certs(true);
            tls_builder.danger_accept_invalid_hostnames(true);
        }
        SslMode::VerifyCa => {
            tls_builder.danger_accept_invalid_hostnames(true);
        }
        SslMode::VerifyFull => {}
        SslMode::Disable => unreachable!(),
    }

    if let Some(ca_path) = &ssl_ca_cert {
        let ca_data = fs::read(ca_path).context("Failed to read CA certificate")?;
        let ca_cert = Certificate::from_pem(&ca_data).context("Failed to parse CA certificate")?;
        tls_builder.add_root_certificate(ca_cert);
    }

    match (&ssl_client_cert, &ssl_client_key) {
        (Some(cert_path), Some(key_path)) => {
            let cert_data = fs::read(cert_path).context("Failed to read client certificate")?;
            let key_data = fs::read(key_path).context("Failed to read client key")?;
            let identity = Identity::from_pkcs12(
                &{
                    let mut combined = cert_data.clone();
                    combined.extend_from_slice(&key_data);
                    combined
                },
                "",
            )
            .or_else(|_| Identity::from_pkcs8(&cert_data, &key_data))
            .context("Failed to parse client certificate and key")?;
            tls_builder.identity(identity);
        }
        (None, None) => {}
        _ => {
            return Err(anyhow::anyhow!(
                "Both --ssl-client-cert and --ssl-client-key must be provided together"
            ));
        }
    }

    let tls = MakeTlsConnector::new(
        tls_builder
            .build()
            .context("Failed to build TLS connector")?,
    );

    let (client, conn) = cfg
        .connect(tls)
        .await
        .context("Failed to connect to PostgreSQL with SSL")?;

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Connection error: {e}");
        }
    });

    Ok(client)
}

/// Set lock_timeout for the current session.
///
/// VACUUM/ANALYZE need ShareUpdateExclusiveLock. With a 10 ms timeout the
/// operation fails fast instead of blocking indefinitely when another process
/// holds a conflicting lock. The caller should treat lock-timeout errors as
/// skipped rather than hard failures.
pub async fn set_lock_timeout(client: &tokio_postgres::Client) -> Result<()> {
    client
        .execute("SET lock_timeout TO '10ms'", &[])
        .await
        .context("Failed to set lock_timeout")?;
    Ok(())
}

/// Set maintenance_work_mem for the current session.
pub async fn set_maintenance_work_mem(
    client: &tokio_postgres::Client,
    maintenance_work_mem_gb: u64,
) -> Result<()> {
    let sql = format!("SET maintenance_work_mem TO '{maintenance_work_mem_gb}GB'");
    client
        .execute(&sql, &[])
        .await
        .context("Failed to set maintenance_work_mem")?;
    Ok(())
}

/// Set vacuum_buffer_usage_limit to 1/16 of shared_buffers (PostgreSQL 16+).
///
/// Returns `(shared_buffers_kb, limit_kb)` so the caller can log both values.
/// Errors on PostgreSQL < 16 where the GUC does not exist.
pub async fn set_vacuum_buffer_usage_limit(client: &tokio_postgres::Client) -> Result<(i64, i64)> {
    let row = client
        .query_one(
            "SELECT pg_size_bytes(current_setting('shared_buffers'))",
            &[],
        )
        .await
        .context("Failed to read shared_buffers")?;

    let shared_buffers_bytes: i64 = row.get(0);
    let shared_buffers_kb = shared_buffers_bytes / 1024;

    // 1/16 of shared_buffers; PostgreSQL requires at least 128 kB.
    const MIN_BYTES: i64 = 128 * 1024;
    let limit_bytes = (shared_buffers_bytes / 16).max(MIN_BYTES);
    let limit_kb = limit_bytes / 1024;

    client
        .execute(
            &format!("SET vacuum_buffer_usage_limit TO '{limit_kb}kB'"),
            &[],
        )
        .await
        .context("Failed to set vacuum_buffer_usage_limit")?;

    Ok((shared_buffers_kb, limit_kb))
}

/// Set max_parallel_maintenance_workers to match the server's max_parallel_workers.
///
/// This lets VACUUM's index-cleanup phase (and CREATE INDEX/REINDEX, if ever run)
/// use as much of the server's parallel worker pool as it's configured to allow,
/// rather than being capped by the (often much lower) built-in default of 2.
/// Note: has no effect on VACUUM calls that pass INDEX_CLEANUP FALSE (Phase 3/freeze),
/// since there's no index-cleanup phase to parallelize in that case.
///
/// Returns the value applied so the caller can log it.
pub async fn set_max_parallel_maintenance_workers(client: &tokio_postgres::Client) -> Result<i32> {
    let row = client
        .query_one("SELECT current_setting('max_parallel_workers')::int", &[])
        .await
        .context("Failed to read max_parallel_workers")?;

    let max_parallel_workers: i32 = row.get(0);

    client
        .execute(
            &format!("SET max_parallel_maintenance_workers TO {max_parallel_workers}"),
            &[],
        )
        .await
        .context("Failed to set max_parallel_maintenance_workers")?;

    Ok(max_parallel_workers)
}

pub fn format_kb_readable(kb: i64) -> String {
    if kb >= 1_048_576 {
        format!("{:.1}GB", kb as f64 / 1_048_576.0)
    } else if kb >= 1024 {
        format!("{:.1}MB", kb as f64 / 1024.0)
    } else {
        format!("{kb}kB")
    }
}
