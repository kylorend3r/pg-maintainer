use anyhow::{Context, Result};
use std::{env, fs, path::Path};

/// Read password from ~/.pgpass (or $PGPASSFILE) for the given connection parameters.
///
/// File format: hostname:port:database:username:password
/// Wildcards ('*') match any value. Returns None when no entry matches.
pub fn get_password_from_pgpass(
    host: &str,
    port: u16,
    database: &str,
    username: &str,
) -> Result<Option<String>> {
    let pgpass_path = env::var("PGPASSFILE").unwrap_or_else(|_| {
        let home = env::var("HOME").unwrap_or_else(|_| env::var("USERPROFILE").unwrap_or_default());
        format!("{}/.pgpass", home)
    });

    let pgpass_path = Path::new(&pgpass_path);

    if !pgpass_path.exists() {
        return Ok(None);
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(pgpass_path)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0);
        if mode != 0o600 {
            eprintln!(
                "WARNING: password file \"{}\" has group or world access; \
                 permissions should be u=rw (0600) or less. Ignored.",
                pgpass_path.display()
            );
            return Ok(None);
        }
    }

    let content = fs::read_to_string(pgpass_path).context("Failed to read .pgpass file")?;

    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let parts = split_pgpass_line(line);
        if parts.len() != 5 {
            continue;
        }

        let host_matches = parts[0].is_empty() || parts[0] == "*" || parts[0] == host;
        let port_matches =
            parts[1].is_empty() || parts[1] == "*" || parts[1] == port.to_string();
        let db_matches = parts[2].is_empty() || parts[2] == "*" || parts[2] == database;
        let user_matches = parts[3].is_empty() || parts[3] == "*" || parts[3] == username;

        if host_matches && port_matches && db_matches && user_matches {
            return Ok(Some(parts[4].clone()));
        }
    }

    Ok(None)
}

fn split_pgpass_line(line: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some(':') => {
                    chars.next();
                    current.push(':');
                }
                Some('\\') => {
                    chars.next();
                    current.push('\\');
                }
                _ => current.push('\\'),
            }
        } else if ch == ':' {
            parts.push(std::mem::take(&mut current));
        } else {
            current.push(ch);
        }
    }
    parts.push(current);
    parts
}
