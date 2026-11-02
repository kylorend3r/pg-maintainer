//! Classification of database errors, so a dead connection can be told apart
//! from an ordinary per-table failure.

use tokio_postgres::error::SqlState;

/// Typed error returned when the connection to the server is lost mid-run.
#[derive(Debug)]
pub struct ConnectionLost {
    pub operation: String,
    pub schema: String,
    pub table: String,
    /// 1-based position of the table within the phase, and the phase's table count.
    pub position: Option<(usize, usize)>,
    pub cause: String,
}

impl std::fmt::Display for ConnectionLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Database connection lost during {} on \"{}\".\"{}\"",
            self.operation, self.schema, self.table
        )?;
        if let Some((i, n)) = self.position {
            write!(f, " (table {i}/{n})")?;
        }
        write!(
            f,
            ": {}. Remaining tables and phases were NOT processed. \
             The advisory lock was released with the session; \
             re-running is safe (candidates are re-discovered).",
            self.cause
        )
    }
}

impl std::error::Error for ConnectionLost {}

/// True for SQLSTATEs that mean the session is gone: class 08 (connection
/// exception) and the 57P01/02/03/05 shutdown and idle-timeout codes.
/// Not included: 57014 (statement timeout / user cancel) and 55P03 (lock timeout).
pub fn sqlstate_is_connection_loss(code: &SqlState) -> bool {
    let c = code.code();
    c.starts_with("08") || matches!(c, "57P01" | "57P02" | "57P03" | "57P05")
}

/// True for I/O error kinds that mean the socket is dead.
pub fn io_kind_is_connection_loss(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        kind,
        BrokenPipe | ConnectionReset | ConnectionAborted | UnexpectedEof | NotConnected | TimedOut
    )
}

/// True when a `tokio_postgres::Error` means the connection is lost.
pub fn is_connection_lost_pg(err: &tokio_postgres::Error) -> bool {
    if err.is_closed() {
        return true;
    }
    if let Some(code) = err.code() {
        return sqlstate_is_connection_loss(code);
    }
    let mut source = std::error::Error::source(err);
    while let Some(s) = source {
        if let Some(io) = s.downcast_ref::<std::io::Error>()
            && io_kind_is_connection_loss(io.kind())
        {
            return true;
        }
        source = s.source();
    }
    false
}

/// True when any error in the chain is a lost connection.
pub fn is_connection_lost(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        if cause.is::<ConnectionLost>() {
            return true;
        }
        if let Some(pg) = cause.downcast_ref::<tokio_postgres::Error>() {
            return is_connection_lost_pg(pg);
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return io_kind_is_connection_loss(io.kind());
        }
        false
    })
}

/// "message (SQLSTATE code)", the form used for server-reported causes.
fn format_server_cause(message: &str, sqlstate: &str) -> String {
    format!("{message} (SQLSTATE {sqlstate})")
}

/// Human-readable cause for a failed statement. `tokio_postgres::Error`'s own
/// `Display` is just "db error" for server-reported errors, so spell out the
/// server's message and SQLSTATE; otherwise append the underlying source.
pub fn describe_pg_error(err: &tokio_postgres::Error) -> String {
    if let Some(db) = err.as_db_error() {
        return format_server_cause(db.message(), db.code().code());
    }
    match std::error::Error::source(err) {
        Some(source) => format!("{err}: {source}"),
        None => err.to_string(),
    }
}

/// Process exit code for a fatal error: 3 for a lost connection, 1 otherwise.
pub fn exit_code_for(err: &anyhow::Error) -> u8 {
    if is_connection_lost(err) { 3 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;

    #[test]
    fn test_sqlstate_connection_loss() {
        for code in [
            "08000", "08003", "08006", "57P01", "57P02", "57P03", "57P05",
        ] {
            assert!(
                sqlstate_is_connection_loss(&SqlState::from_code(code)),
                "{code} should be connection loss"
            );
        }
        for code in ["57014", "55P03", "40001"] {
            assert!(
                !sqlstate_is_connection_loss(&SqlState::from_code(code)),
                "{code} should not be connection loss"
            );
        }
    }

    #[test]
    fn test_io_kind_connection_loss() {
        use std::io::ErrorKind::*;
        for k in [
            BrokenPipe,
            ConnectionReset,
            ConnectionAborted,
            UnexpectedEof,
            NotConnected,
            TimedOut,
        ] {
            assert!(io_kind_is_connection_loss(k), "{k:?}");
        }
        for k in [PermissionDenied, NotFound, InvalidInput] {
            assert!(!io_kind_is_connection_loss(k), "{k:?}");
        }
    }

    fn lost() -> ConnectionLost {
        ConnectionLost {
            operation: "VACUUM".into(),
            schema: "public".into(),
            table: "big".into(),
            position: Some((2, 5)),
            cause: "connection closed".into(),
        }
    }

    #[test]
    fn test_connection_lost_display() {
        let msg = lost().to_string();
        assert!(msg.contains("\"public\".\"big\""));
        assert!(msg.contains("(table 2/5)"));
        assert!(msg.contains("NOT processed"));
    }

    #[test]
    fn test_exit_code_for_connection_lost_with_context() {
        let err = anyhow::Error::new(lost()).context("phase failed");
        assert_eq!(exit_code_for(&err), 3);
    }

    #[test]
    fn test_exit_code_for_io_error_in_chain() {
        let io = std::io::Error::from(std::io::ErrorKind::BrokenPipe);
        let err = Err::<(), _>(io).context("query failed").unwrap_err();
        assert_eq!(exit_code_for(&err), 3);
    }

    #[test]
    fn test_format_server_cause() {
        assert_eq!(
            format_server_cause(
                "terminating connection due to administrator command",
                "57P01"
            ),
            "terminating connection due to administrator command (SQLSTATE 57P01)"
        );
    }

    #[test]
    fn test_exit_code_for_other_error() {
        assert_eq!(exit_code_for(&anyhow::anyhow!("something else")), 1);
    }
}
