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
use serde::Serialize;
use sqlx::FromRow;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePool};
use tokio::time::Instant;
use tracing::warn;

/// Where the totals are read.
const TOTALS_PATH: &str = "/transfer-stats";

/// Tokens kept before the expired ones are dropped.
const MAX_TOKENS: usize = 1_000_000;

/// Each receiver's report of the bytes it got and how, made with the one-time
/// token the relay handed it. Rows keep only the hour, so none can be matched
/// to a connection.
pub(crate) struct Stats {
    db: SqlitePool,
    /// Tokens not used yet, and when they expire; a restart forgets them.
    tokens: Mutex<HashMap<String, Instant>>,
    token_ttl: Duration,
}

/// Bytes received in windows that reach back from now, and today since 00:00 UTC.
/// Each counts the whole hours it touches, so it may hold up to an hour more.
#[derive(Debug, Serialize, FromRow)]
struct Totals {
    last_hour: i64,
    today: i64,
    last_day: i64,
    last_7_days: i64,
    last_month: i64,
    last_year: i64,
}

impl Stats {
    /// Opens or creates the database at `path` and brings its tables up to date.
    pub(crate) async fn open(path: &Path, token_ttl: Duration) -> io::Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let db = SqlitePool::connect_with(options)
            .await
            .map_err(io::Error::other)?;
        sqlx::migrate!().run(&db).await.map_err(io::Error::other)?;
        Ok(Self {
            db,
            tokens: Mutex::default(),
            token_ttl,
        })
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
    async fn count(&self, report: &Report) -> sqlx::Result<bool> {
        let expires = self.tokens().remove(&report.token);
        if !expires.is_some_and(|expires| expires > Instant::now()) {
            return Ok(false);
        }
        sqlx::query("INSERT INTO transfers VALUES (strftime('%Y-%m-%d %H:00', 'now'), ?, ?, ?, ?)")
            .bind(name(report.platform))
            .bind(name(report.transport))
            .bind(name(report.route))
            .bind(i64::try_from(report.bytes).unwrap_or(i64::MAX))
            .execute(&self.db)
            .await?;
        Ok(true)
    }

    async fn totals(&self) -> sqlx::Result<Totals> {
        // Rows are stamped with the hour they began, so an hour touches the
        // window if it began less than an hour before the window starts.
        sqlx::query_as(
            "SELECT
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?1, '-1 hour')), 0) AS last_hour,
                coalesce(sum(bytes) FILTER (WHERE hour >= date('now')), 0) AS today,
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?2, '-1 hour')), 0) AS last_day,
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?3, '-1 hour')), 0) AS last_7_days,
                coalesce(sum(bytes) FILTER (WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?4, '-1 hour')), 0) AS last_month,
                coalesce(sum(bytes), 0) AS last_year
             FROM transfers WHERE hour > strftime('%Y-%m-%d %H:%M', 'now', ?5, '-1 hour')",
        )
        .bind("-1 hours")
        .bind("-24 hours")
        .bind("-7 days")
        .bind("-30 days")
        .bind("-365 days")
        .fetch_one(&self.db)
        .await
    }
}

pub(crate) fn router(stats: Arc<Stats>) -> Router {
    Router::new()
        .route(PATH, post(report))
        .route(TOTALS_PATH, get(totals))
        .with_state(stats)
}

async fn totals(State(stats): State<Arc<Stats>>) -> Result<Json<Totals>, StatusCode> {
    stats.totals().await.map(Json).map_err(|error| {
        warn!(?error, "reading the stats totals failed");
        StatusCode::INTERNAL_SERVER_ERROR
    })
}

/// Reads JSON whatever the content type: browsers send beacons as text.
async fn report(State(stats): State<Arc<Stats>>, body: Bytes) -> StatusCode {
    let Ok(report) = serde_json::from_slice::<Report>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    match stats.count(&report).await {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::FORBIDDEN,
        Err(error) => {
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
