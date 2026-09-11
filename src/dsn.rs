//! Parsing for `--dsn` / `PG_DSN` connection strings.
//!
//! Accepts either a `postgres://`/`postgresql://` URI or a libpq keyword string
//! (`host=... dbname=...`), and translates it into the same fields the individual
//! connection flags produce.
//!
//! `tokio_postgres::Config::from_str` does the heavy lifting, but it cannot be
//! handed the string as-is: it rejects `sslmode=verify-ca`/`verify-full` and the
//! `sslrootcert`/`sslcert`/`sslkey` parameters outright, because certificate
//! handling belongs to the TLS connector rather than to the protocol layer. Those
//! four parameters map one-to-one onto flags pg-maintainer already has, so we
//! extract them here and pass the remainder through.

use crate::types::SslMode;
use anyhow::{Context, Result, anyhow};
use std::str::FromStr;
use tokio_postgres::Config;

/// The SSL parameters we handle ourselves because tokio-postgres will not.
const SSL_PARAMS: [&str; 4] = ["sslmode", "sslrootcert", "sslcert", "sslkey"];

/// Parameters tokio-postgres accepts but pg-maintainer does not act on.
/// Collected and reported so they are not silently dropped.
const UNUSED_PARAMS: [&str; 6] = [
    "application_name",
    "options",
    "target_session_attrs",
    "keepalives",
    "keepalives_idle",
    "channel_binding",
];

/// Connection fields recovered from a DSN. Every field is optional: a DSN only
/// fills in what it actually specified.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedDsn {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub database: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub sslmode: Option<SslMode>,
    pub ssl_ca_cert: Option<String>,
    pub ssl_client_cert: Option<String>,
    pub ssl_client_key: Option<String>,
    pub connect_timeout_seconds: Option<u64>,
    /// Parameters that parsed fine but that this tool ignores.
    pub ignored_params: Vec<String>,
}

/// True when the string looks like a `postgres://` / `postgresql://` URI.
fn is_uri(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    lower.starts_with("postgres://") || lower.starts_with("postgresql://")
}

/// Decode `%XX` escapes. Invalid escapes are left as-is, matching the lenient
/// behavior of most URI consumers.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Split a libpq keyword string into `(key, value)` pairs.
///
/// Follows libpq's rules: pairs are whitespace-separated, `=` may be surrounded by
/// whitespace, and a value may be single-quoted, in which case `\'` and `\\` are
/// escapes and whitespace is literal.
fn split_keyword_pairs(s: &str) -> Result<Vec<(String, String)>> {
    let chars: Vec<char> = s.chars().collect();
    let mut pairs = Vec::new();
    let mut i = 0;

    while i < chars.len() {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }

        // key
        let key_start = i;
        while i < chars.len() && chars[i] != '=' && !chars[i].is_whitespace() {
            i += 1;
        }
        let key: String = chars[key_start..i].iter().collect();
        if key.is_empty() {
            return Err(anyhow!(
                "malformed keyword/value string: empty parameter name"
            ));
        }

        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            return Err(anyhow!(
                "malformed keyword/value string: parameter '{key}' has no '=' value"
            ));
        }
        i += 1; // consume '='
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }

        // value
        let mut value = String::new();
        if i < chars.len() && chars[i] == '\'' {
            i += 1;
            let mut closed = false;
            while i < chars.len() {
                match chars[i] {
                    '\\' if i + 1 < chars.len() => {
                        value.push(chars[i + 1]);
                        i += 2;
                    }
                    '\'' => {
                        i += 1;
                        closed = true;
                        break;
                    }
                    c => {
                        value.push(c);
                        i += 1;
                    }
                }
            }
            if !closed {
                return Err(anyhow!(
                    "malformed keyword/value string: unterminated quoted value for '{key}'"
                ));
            }
        } else {
            while i < chars.len() && !chars[i].is_whitespace() {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    value.push(chars[i + 1]);
                    i += 2;
                } else {
                    value.push(chars[i]);
                    i += 1;
                }
            }
        }
        pairs.push((key, value));
    }

    Ok(pairs)
}

/// Quote a value for a libpq keyword string, mirroring `connection::escape_libpq_value`.
fn escape_keyword_value(s: &str) -> String {
    if s.is_empty()
        || s.chars()
            .any(|c| c.is_whitespace() || c == '\'' || c == '\\')
    {
        let escaped = s.replace('\\', "\\\\").replace('\'', "\\'");
        format!("'{escaped}'")
    } else {
        s.to_owned()
    }
}

/// The authority section of a URI: everything between `//` and the next `/` or `?`.
fn uri_authority(s: &str) -> &str {
    let after_scheme = match s.find("//") {
        Some(i) => &s[i + 2..],
        None => return "",
    };
    let end = after_scheme.find(['/', '?']).unwrap_or(after_scheme.len());
    &after_scheme[..end]
}

/// Whether a URI's authority names an explicit port.
///
/// Needed because `Config::from_str` fills in 5432 for a URI that omits the port,
/// so the parsed value cannot distinguish "given" from "defaulted". Handles
/// userinfo (split at the last `@`) and bracketed IPv6 literals.
fn uri_has_explicit_port(s: &str) -> bool {
    let authority = uri_authority(s);
    let hostpart = match authority.rfind('@') {
        Some(i) => &authority[i + 1..],
        None => authority,
    };
    match hostpart.rfind(']') {
        // bracketed IPv6: a port can only follow the closing bracket
        Some(close) => hostpart[close + 1..].starts_with(':'),
        None => hostpart.contains(':'),
    }
}

/// A DSN split into the SSL parameters this tool handles itself and the remainder
/// that `tokio_postgres::Config` can parse.
struct SplitDsn {
    ssl: Vec<(String, String)>,
    /// The DSN with the SSL parameters removed.
    rest: String,
    /// Every non-SSL parameter key seen, for the "ignored parameter" report.
    other_keys: Vec<String>,
}

/// Pull the SSL parameters out of a DSN.
///
/// Non-SSL parts are preserved exactly: URI query segments are kept verbatim so
/// percent-encoding is never rewritten.
fn extract_ssl_params(raw: &str) -> Result<SplitDsn> {
    let mut ssl = Vec::new();
    let mut other_keys = Vec::new();

    if is_uri(raw) {
        let (base, query) = match raw.split_once('?') {
            Some((b, q)) => (b, q),
            None => {
                return Ok(SplitDsn {
                    ssl,
                    rest: raw.to_string(),
                    other_keys,
                });
            }
        };

        let mut kept: Vec<&str> = Vec::new();
        for segment in query.split('&') {
            if segment.is_empty() {
                continue;
            }
            let (raw_key, raw_value) = match segment.split_once('=') {
                Some((k, v)) => (k, v),
                None => (segment, ""),
            };
            let key = percent_decode(raw_key).to_ascii_lowercase();
            if SSL_PARAMS.contains(&key.as_str()) {
                ssl.push((key, percent_decode(raw_value)));
            } else {
                other_keys.push(key);
                kept.push(segment);
            }
        }

        let rest = if kept.is_empty() {
            base.to_string()
        } else {
            format!("{base}?{}", kept.join("&"))
        };
        Ok(SplitDsn {
            ssl,
            rest,
            other_keys,
        })
    } else {
        let pairs = split_keyword_pairs(raw)?;
        let mut kept = Vec::new();
        for (key, value) in pairs {
            let lower = key.to_ascii_lowercase();
            if SSL_PARAMS.contains(&lower.as_str()) {
                ssl.push((lower, value));
            } else {
                other_keys.push(lower);
                kept.push(format!("{key}={}", escape_keyword_value(&value)));
            }
        }
        Ok(SplitDsn {
            ssl,
            rest: kept.join(" "),
            other_keys,
        })
    }
}

/// Map a libpq `sslmode` value onto the tool's own SSL mode.
///
/// `allow` and `prefer` are rejected rather than approximated: this tool either
/// connects in plaintext or requires TLS, with no opportunistic middle ground.
/// Guessing either way would silently change the security of the connection.
fn map_sslmode(value: &str) -> Result<SslMode> {
    match value.to_ascii_lowercase().as_str() {
        "disable" => Ok(SslMode::Disable),
        "require" => Ok(SslMode::Require),
        "verify-ca" => Ok(SslMode::VerifyCa),
        "verify-full" => Ok(SslMode::VerifyFull),
        "allow" | "prefer" => Err(anyhow!(
            "DSN sslmode '{value}' is not supported — pg-maintainer connects either \
             in plaintext or with TLS required, with no opportunistic mode. \
             Use one of: disable, require, verify-ca, verify-full"
        )),
        other => Err(anyhow!(
            "DSN has an invalid sslmode '{other}'. Must be one of: \
             disable, require, verify-ca, verify-full"
        )),
    }
}

/// Parse a `--dsn` / `PG_DSN` value into its connection fields.
pub fn parse(raw: &str) -> Result<ParsedDsn> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("--dsn value is empty"));
    }
    if !is_uri(trimmed) && !trimmed.contains('=') {
        return Err(anyhow!(
            "could not parse --dsn: expected a postgres:// URI or a libpq \
             keyword string such as \"host=db.internal dbname=mydb\""
        ));
    }

    let split = extract_ssl_params(trimmed)?;

    let cfg = Config::from_str(&split.rest).map_err(|e| {
        anyhow!(
            "could not parse --dsn: {e}. Expected a postgres:// URI or a libpq \
             keyword string; note that the password must be percent-encoded in a URI"
        )
    })?;

    let hosts = cfg.get_hosts();
    if hosts.len() > 1 {
        return Err(anyhow!(
            "--dsn names {} hosts — pg-maintainer connects to a single host, \
             so multi-host (failover) connection strings are not supported",
            hosts.len()
        ));
    }

    let host = hosts.first().map(|h| match h {
        tokio_postgres::config::Host::Tcp(name) => name.clone(),
        tokio_postgres::config::Host::Unix(path) => path.to_string_lossy().into_owned(),
    });

    // A URI always yields a port (the parser substitutes 5432), so only trust it
    // when the string actually named one. The keyword form reports it faithfully.
    let port = if is_uri(trimmed) && !uri_has_explicit_port(trimmed) {
        None
    } else {
        cfg.get_ports().first().copied()
    };

    let mut parsed = ParsedDsn {
        host,
        port,
        database: cfg.get_dbname().map(str::to_owned),
        username: cfg.get_user().map(str::to_owned),
        password: cfg
            .get_password()
            .map(|p| String::from_utf8_lossy(p).into_owned()),
        connect_timeout_seconds: cfg.get_connect_timeout().map(|d| d.as_secs()),
        ignored_params: split
            .other_keys
            .into_iter()
            .filter(|k| UNUSED_PARAMS.contains(&k.as_str()))
            .collect(),
        ..Default::default()
    };

    for (key, value) in split.ssl {
        match key.as_str() {
            "sslmode" => parsed.sslmode = Some(map_sslmode(&value)?),
            "sslrootcert" => parsed.ssl_ca_cert = Some(value),
            "sslcert" => parsed.ssl_client_cert = Some(value),
            "sslkey" => parsed.ssl_client_key = Some(value),
            _ => unreachable!("SSL_PARAMS and this match must stay in sync"),
        }
    }

    Ok(parsed)
}

/// Render a DSN with its password masked, safe to write to a log.
///
/// Deliberately conservative: anything this cannot confidently rewrite comes back
/// as `[REDACTED]` rather than risking a leak.
pub fn redact(raw: &str) -> String {
    const MASK: &str = "***";
    let trimmed = raw.trim();

    if is_uri(trimmed) {
        let authority = uri_authority(trimmed);
        let Some(at) = authority.rfind('@') else {
            return trimmed.to_string(); // no userinfo, so no password
        };
        let userinfo = &authority[..at];
        let Some(colon) = userinfo.find(':') else {
            return trimmed.to_string(); // user only, no password
        };
        let masked_authority = format!("{}:{MASK}{}", &userinfo[..colon], &authority[at..]);
        return trimmed.replacen(authority, &masked_authority, 1);
    }

    match split_keyword_pairs(trimmed) {
        Ok(pairs) => pairs
            .into_iter()
            .map(|(key, value)| {
                if key.eq_ignore_ascii_case("password") {
                    format!("{key}={MASK}")
                } else {
                    format!("{key}={}", escape_keyword_value(&value))
                }
            })
            .collect::<Vec<_>>()
            .join(" "),
        Err(_) => "[REDACTED]".to_string(),
    }
}

/// Resolve the DSN string itself: `--dsn` (or the config file) first, then `PG_DSN`.
pub fn resolve_dsn_source(explicit: Option<String>) -> Result<Option<String>> {
    if let Some(value) = explicit {
        return Ok(Some(value));
    }
    match std::env::var("PG_DSN") {
        Ok(v) if !v.trim().is_empty() => Ok(Some(v)),
        Ok(_) => Ok(None),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(e).context("Failed to read PG_DSN"),
    }
}
