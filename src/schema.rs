//! Maintenance logbook schema creation and management.

use anyhow::{Context, Result};
use tokio_postgres::Client;

const LOGBOOK_SCHEMA: &str = "maintainer_logbook";

const CREATE_SCHEMA_SQL: &str = "CREATE SCHEMA maintainer_logbook";

const CREATE_LOGBOOK_TABLE_SQL: &str = r#"
    CREATE TABLE IF NOT EXISTS maintainer_logbook.maintenance_logbook (
        id                 BIGSERIAL PRIMARY KEY,
        run_started_at     TIMESTAMPTZ NOT NULL,
        schema_name        VARCHAR(255) NOT NULL,
        table_name         VARCHAR(255) NOT NULL,
        operation          VARCHAR(32)  NOT NULL,
        mode               VARCHAR(32)  NOT NULL,
        status             VARCHAR(16)  NOT NULL,
        dead_tuples_before BIGINT,
        dead_tuples_removed BIGINT,
        duration_ms        BIGINT,
        error_message      TEXT,
        logged_at          TIMESTAMPTZ NOT NULL DEFAULT now()
    )
"#;

const CREATE_INDEX_TABLE_SQL: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_maintenance_logbook_table
        ON maintainer_logbook.maintenance_logbook (schema_name, table_name)
"#;

const CREATE_INDEX_TIME_SQL: &str = r#"
    CREATE INDEX IF NOT EXISTS idx_maintenance_logbook_logged_at
        ON maintainer_logbook.maintenance_logbook (logged_at)
"#;

/// Check if a schema exists in the target database.
async fn schema_exists(client: &Client, schema_name: &str) -> Result<bool> {
    let row = client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM pg_namespace WHERE nspname = $1)",
            &[&schema_name],
        )
        .await
        .context("Failed to check if schema exists")?;
    Ok(row.get(0))
}

/// Create the maintainer_logbook schema and tables if they don't exist.
/// Returns true if the schema was created, false if it already existed.
pub async fn ensure_logbook_schema(client: &Client) -> Result<bool> {
    let exists = schema_exists(client, LOGBOOK_SCHEMA)
        .await
        .context("Failed to check logbook schema existence")?;

    if !exists {
        client
            .execute(CREATE_SCHEMA_SQL, &[])
            .await
            .context("Failed to create logbook schema")?;
    }

    client
        .execute(CREATE_LOGBOOK_TABLE_SQL, &[])
        .await
        .context("Failed to create logbook table")?;

    client
        .execute(CREATE_INDEX_TABLE_SQL, &[])
        .await
        .context("Failed to create logbook table index")?;

    client
        .execute(CREATE_INDEX_TIME_SQL, &[])
        .await
        .context("Failed to create logbook timestamp index")?;

    Ok(!exists)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logbook_schema_name() {
        assert_eq!(LOGBOOK_SCHEMA, "maintainer_logbook");
    }
}
