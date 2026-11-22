use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use fsend_proto::stats::{PATH, Report};
use rusqlite::{Connection, params};
use serde::Serialize;
use tokio::time::Instant;
use tracing::warn;

/// Where the totals are read.
const TOTALS_PATH: &str = "/transfer-stats";

/// Tokens kept before the expired ones are dropped.
const MAX_TOKENS: usize = 1_000_000;

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS transfers (
        hour TEXT NOT NULL,
        platform TEXT NOT NULL,
        transport TEXT NOT NULL,
        route TEXT NOT NULL,
        bytes INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS transfers_hour ON transfers (hour);
";

/// Each receiver's report of the bytes it got and how, made with the one-time
/// token the relay handed it. Rows keep only the hour, so none can be matched
/// to a connection.
pub(crate) struct Stats {
    db: Mutex<Connection>,
    /// Tokens not used yet, and when they expire; a restart forgets them.
    tokens: Mutex<HashMap<String, Instant>>,
    token_ttl: Duration,
}

/// Bytes received in windows that reach back from now, and today since 00:00 UTC.
/// Each counts the whole hours it touches, so it may hold up to an hour more.
#[derive(Debug, Serialize)]
struct Totals {
    last_hour: i64,
    today: i64,
    last_day: i64,
    last_7_days: i64,
    last_month: i64,
    last_year: i64,
}

impl Stats {
    pub(crate) fn open(path: &Path, token_ttl: Duration) -> io::Result<Self> {
        let db = Connection::open(path).map_err(io::Error::other)?;
        db.execute_batch(SCHEMA).map_err(io::Error::other)?;
        Ok(Self {
            db: Mutex::new(db),
            tokens: Mutex::default(),
            token_ttl,
        })
    }

    fn db(&self) -> MutexGuard<'_, Connection> {
        self.db.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn tokens(&self) -> MutexGuard<'_, HashMap<String, Instant>> {
        self.tokens.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A token for one report.
    pub(crate) fn issue(&self) -> Option<String> {
        let mut tokens = self.tokens();
        let now = Instant::now();
        if tokens.len() >= MAX_TOKENS {
            tokens.retain(|_, expires| *expires > now);
            if tokens.len() >= MAX_TOKENS {
                return None;
            }
        }
        let token = token();
        tokens.insert(token.clone(), now + self.token_ttl);
        Some(token)
    }

    /// Counts `report` and uses up its token; false if the token is not valid.
    fn count(&self, report: &Report) -> rusqlite::Result<bool> {
        let expires = self.tokens().remove(&report.token);
        if !expires.is_some_and(|expires| expires > Instant::now()) {
            return Ok(false);
        }
        self.db().execute(
            "INSERT INTO transfers VALUES (strftime('%Y-%m-%d %H:00', 'now'), ?1, ?2, ?3, ?4)",
            params![
                name(report.platform),
                name(report.transport),
                name(report.route),
                i64::try_from(report.bytes).unwrap_or(i64::MAX)
            ],
        )?;
        Ok(true)
    }

    fn totals(&self) -> rusqlite::Result<Totals> {
        // Rows are stamped with the hour they began, so an hour touches the
        // window if it began less than an hour before the window starts.
        self.db().query_row(
            "SELECT
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?1, '-1 hour')), 0),
                coalesce(sum(bytes) FILTER (WHERE hour >= date('now')), 0),
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?2, '-1 hour')), 0),
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?3, '-1 hour')), 0),
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?4, '-1 hour')), 0),
                coalesce(sum(bytes), 0)
             FROM transfers WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?5, '-1 hour')",
            ["-1 hours", "-24 hours", "-7 days", "-30 days", "-365 days"],
            |row| {
                Ok(Totals {
                    last_hour: row.get(0)?,
                    today: row.get(1)?,
                    last_day: row.get(2)?,
                    last_7_days: row.get(3)?,
                    last_month: row.get(4)?,
                    last_year: row.get(5)?,
                })
            },
        )
    }
}

pub(crate) fn router(stats: Arc<Stats>) -> Router {
    Router::new()
        .route(PATH, post(report))
        .route(TOTALS_PATH, get(totals))
        .with_state(stats)
}

async fn totals(State(stats): State<Arc<Stats>>) -> Result<Json<Totals>, StatusCode> {
    match tokio::task::spawn_blocking(move || stats.totals()).await {
        Ok(Ok(totals)) => Ok(Json(totals)),
        error => {
            warn!(?error, "reading the stats totals failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// Reads JSON whatever the content type: browsers send beacons as text.
async fn report(State(stats): State<Arc<Stats>>, body: Bytes) -> StatusCode {
    let Ok(report) = serde_json::from_slice::<Report>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    match tokio::task::spawn_blocking(move || stats.count(&report)).await {
        Ok(Ok(true)) => StatusCode::NO_CONTENT,
        Ok(Ok(false)) => StatusCode::FORBIDDEN,
        error => {
            warn!(?error, "counting a stats report failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// The wire name of a unit variant, as the database stores it.
fn name(variant: impl Serialize) -> String {
    match serde_json::to_value(variant) {
        Ok(serde_json::Value::String(name)) => name,
        _ => unreachable!("unit variants serialize to their names"),
    }
}

fn token() -> String {
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes).expect("the system random number generator is unavailable");
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
