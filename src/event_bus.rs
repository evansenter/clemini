//! Event bus for cross-session coordination.
//!
//! Provides pub-sub messaging between clemini sessions using SQLite for persistence.
//! Inspired by claude-event-bus but implemented in Rust.
//!
//! # Features
//!
//! - Session registration with heartbeats
//! - Channel-based pub-sub: `all`, `session:<id>`, `repo:<name>`, `machine:<name>`
//! - Cursor-based event streaming
//! - Automatic session expiration (5 min without heartbeat)
//!
//! # Example
//!
//! ```ignore
//! let bus = EventBus::open()?;
//! let session_id = bus.register_session("my-branch", None, None, None)?;
//! bus.publish_event("task_completed", "Done!", Some(&session_id), "all")?;
//! let events = bus.get_events(None, 50, Some(&session_id), "desc", None, false, None)?;
//! ```

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// Session heartbeat timeout in seconds (5 minutes).
const SESSION_TIMEOUT_SECS: i64 = 300;

/// Event bus error types.
#[derive(Debug, thiserror::Error)]
pub enum EventBusError {
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("Session not found: {0}")]
    SessionNotFound(String),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, EventBusError>;

/// A registered session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub name: String,
    pub machine: String,
    pub cwd: String,
    pub client_id: Option<String>,
    pub cursor: i64,
    pub last_heartbeat: i64,
    pub created_at: i64,
}

/// An event in the bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: i64,
    pub event_type: String,
    pub payload: String,
    pub channel: String,
    pub session_id: Option<String>,
    pub created_at: i64,
}

/// Channel info with subscriber count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    pub subscriber_count: i64,
}

/// Event bus backed by SQLite.
pub struct EventBus {
    conn: Mutex<Connection>,
}

impl EventBus {
    /// Open the event bus, creating the database if needed.
    pub fn open() -> Result<Self> {
        let db_path = Self::db_path()?;

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;
        let bus = Self {
            conn: Mutex::new(conn),
        };
        bus.init_schema()?;
        Ok(bus)
    }

    /// Open an in-memory event bus (for testing).
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let bus = Self {
            conn: Mutex::new(conn),
        };
        bus.init_schema()?;
        Ok(bus)
    }

    /// Get the database path.
    fn db_path() -> Result<PathBuf> {
        let home = dirs::home_dir().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "Home directory not found")
        })?;
        Ok(home.join(".clemini").join("event_bus.db"))
    }

    /// Initialize the database schema.
    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                machine TEXT NOT NULL,
                cwd TEXT NOT NULL,
                client_id TEXT,
                cursor INTEGER DEFAULT 0,
                last_heartbeat INTEGER NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                channel TEXT NOT NULL,
                session_id TEXT,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_channel ON events(channel);
            CREATE INDEX IF NOT EXISTS idx_events_created ON events(created_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_machine_client ON sessions(machine, client_id);
            ",
        )?;

        Ok(())
    }

    /// Get current Unix timestamp.
    fn now() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    }

    /// Generate a unique session ID.
    fn generate_session_id() -> String {
        uuid_v4()
    }

    /// Register a new session.
    ///
    /// If `client_id` is provided and a session with the same (machine, client_id) exists,
    /// that session is resumed instead of creating a new one.
    pub fn register_session(
        &self,
        name: &str,
        machine: Option<&str>,
        cwd: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<Session> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();

        let machine = machine.map(String::from).unwrap_or_else(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string())
        });

        let cwd = cwd.map(String::from).unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| ".".to_string())
        });

        // Check for existing session to resume
        if let Some(client_id) = client_id {
            let mut stmt = conn.prepare(
                "SELECT id, name, machine, cwd, client_id, cursor, last_heartbeat, created_at
                 FROM sessions WHERE machine = ?1 AND client_id = ?2",
            )?;

            if let Ok(session) = stmt.query_row(params![&machine, client_id], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    machine: row.get(2)?,
                    cwd: row.get(3)?,
                    client_id: row.get(4)?,
                    cursor: row.get(5)?,
                    last_heartbeat: row.get(6)?,
                    created_at: row.get(7)?,
                })
            }) {
                // Update heartbeat and return existing session
                conn.execute(
                    "UPDATE sessions SET last_heartbeat = ?1, name = ?2, cwd = ?3 WHERE id = ?4",
                    params![now, name, &cwd, &session.id],
                )?;
                return Ok(Session {
                    last_heartbeat: now,
                    name: name.to_string(),
                    cwd,
                    ..session
                });
            }
        }

        // Create new session
        let id = Self::generate_session_id();
        conn.execute(
            "INSERT INTO sessions (id, name, machine, cwd, client_id, cursor, last_heartbeat, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?6)",
            params![&id, name, &machine, &cwd, client_id, now],
        )?;

        Ok(Session {
            id,
            name: name.to_string(),
            machine,
            cwd,
            client_id: client_id.map(String::from),
            cursor: 0,
            last_heartbeat: now,
            created_at: now,
        })
    }

    /// Unregister a session.
    pub fn unregister_session(
        &self,
        session_id: Option<&str>,
        client_id: Option<&str>,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();

        let rows = if let Some(session_id) = session_id {
            conn.execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?
        } else if let Some(client_id) = client_id {
            let machine = hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown".to_string());
            conn.execute(
                "DELETE FROM sessions WHERE machine = ?1 AND client_id = ?2",
                params![&machine, client_id],
            )?
        } else {
            return Ok(false);
        };

        Ok(rows > 0)
    }

    /// Update session heartbeat (keeps session alive).
    pub fn heartbeat(&self, session_id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();
        let rows = conn.execute(
            "UPDATE sessions SET last_heartbeat = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;
        Ok(rows > 0)
    }

    /// List active sessions (with valid heartbeats).
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Self::now() - SESSION_TIMEOUT_SECS;

        // Clean up expired sessions
        conn.execute(
            "DELETE FROM sessions WHERE last_heartbeat < ?1",
            params![cutoff],
        )?;

        let mut stmt = conn.prepare(
            "SELECT id, name, machine, cwd, client_id, cursor, last_heartbeat, created_at
             FROM sessions
             ORDER BY last_heartbeat DESC",
        )?;

        let sessions = stmt
            .query_map([], |row| {
                Ok(Session {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    machine: row.get(2)?,
                    cwd: row.get(3)?,
                    client_id: row.get(4)?,
                    cursor: row.get(5)?,
                    last_heartbeat: row.get(6)?,
                    created_at: row.get(7)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(sessions)
    }

    /// List channels with subscriber counts.
    pub fn list_channels(&self) -> Result<Vec<ChannelInfo>> {
        let conn = self.conn.lock().unwrap();

        // Get unique channels from recent events
        let mut stmt = conn.prepare("SELECT DISTINCT channel FROM events ORDER BY channel")?;

        let channels: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // For each channel, count "subscribers" (sessions that might be listening)
        // This is a simplification - in practice, sessions subscribe by polling
        let sessions = self.list_sessions()?;

        let mut result = vec![];
        for channel in channels {
            let count = if channel == "all" {
                sessions.len() as i64
            } else if channel.starts_with("session:") {
                1 // Direct session channels have 1 subscriber
            } else if channel.starts_with("repo:") {
                // Count sessions in that repo's cwd
                let repo = channel.strip_prefix("repo:").unwrap_or("");
                sessions
                    .iter()
                    .filter(|s| s.cwd.contains(repo) || s.name.contains(repo))
                    .count() as i64
            } else if channel.starts_with("machine:") {
                let machine = channel.strip_prefix("machine:").unwrap_or("");
                sessions.iter().filter(|s| s.machine == machine).count() as i64
            } else {
                0
            };

            result.push(ChannelInfo {
                name: channel,
                subscriber_count: count,
            });
        }

        Ok(result)
    }

    /// Publish an event to a channel.
    pub fn publish_event(
        &self,
        event_type: &str,
        payload: &str,
        session_id: Option<&str>,
        channel: &str,
    ) -> Result<Event> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();

        // Update heartbeat if session provided
        if let Some(sid) = session_id {
            conn.execute(
                "UPDATE sessions SET last_heartbeat = ?1 WHERE id = ?2",
                params![now, sid],
            )?;
        }

        conn.execute(
            "INSERT INTO events (event_type, payload, channel, session_id, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![event_type, payload, channel, session_id, now],
        )?;

        let id = conn.last_insert_rowid();

        Ok(Event {
            id,
            event_type: event_type.to_string(),
            payload: payload.to_string(),
            channel: channel.to_string(),
            session_id: session_id.map(String::from),
            created_at: now,
        })
    }

    /// Get events with optional filtering.
    ///
    /// # Arguments
    /// - `cursor`: Start from this event ID (exclusive)
    /// - `limit`: Maximum events to return
    /// - `session_id`: Session requesting events (for cursor tracking)
    /// - `order`: "asc" or "desc"
    /// - `channel`: Filter to specific channel
    /// - `resume`: Use session's saved cursor
    /// - `event_types`: Filter by event types
    #[allow(clippy::too_many_arguments)]
    pub fn get_events(
        &self,
        cursor: Option<i64>,
        limit: usize,
        session_id: Option<&str>,
        order: &str,
        channel: Option<&str>,
        resume: bool,
        event_types: Option<&[String]>,
    ) -> Result<(Vec<Event>, Option<i64>)> {
        let conn = self.conn.lock().unwrap();
        let now = Self::now();

        // Update heartbeat if session provided
        if let Some(sid) = session_id {
            conn.execute(
                "UPDATE sessions SET last_heartbeat = ?1 WHERE id = ?2",
                params![now, sid],
            )?;
        }

        // Get cursor from session if resuming
        let effective_cursor = if resume {
            if let Some(sid) = session_id {
                let saved: Option<i64> = conn
                    .query_row(
                        "SELECT cursor FROM sessions WHERE id = ?1",
                        params![sid],
                        |row| row.get(0),
                    )
                    .ok();
                saved.or(cursor)
            } else {
                cursor
            }
        } else {
            cursor
        };

        // Build query
        let order_dir = if order == "asc" { "ASC" } else { "DESC" };
        let cursor_op = if order == "asc" { ">" } else { "<" };

        let mut sql = String::from(
            "SELECT id, event_type, payload, channel, session_id, created_at FROM events WHERE 1=1",
        );
        let mut params_vec: Vec<Box<dyn rusqlite::ToSql>> = vec![];

        if let Some(c) = effective_cursor {
            sql.push_str(&format!(" AND id {} ?", cursor_op));
            params_vec.push(Box::new(c));
        }

        if let Some(ch) = channel {
            sql.push_str(" AND channel = ?");
            params_vec.push(Box::new(ch.to_string()));
        }

        if let Some(types) = event_types
            && !types.is_empty()
        {
            let placeholders: Vec<String> = types.iter().map(|_| "?".to_string()).collect();
            sql.push_str(&format!(" AND event_type IN ({})", placeholders.join(",")));
            for t in types {
                params_vec.push(Box::new(t.clone()));
            }
        }

        sql.push_str(&format!(" ORDER BY id {} LIMIT ?", order_dir));
        params_vec.push(Box::new(limit as i64));

        let params_refs: Vec<&dyn rusqlite::ToSql> =
            params_vec.iter().map(|p| p.as_ref()).collect();

        let mut stmt = conn.prepare(&sql)?;
        let events: Vec<Event> = stmt
            .query_map(params_refs.as_slice(), |row| {
                Ok(Event {
                    id: row.get(0)?,
                    event_type: row.get(1)?,
                    payload: row.get(2)?,
                    channel: row.get(3)?,
                    session_id: row.get(4)?,
                    created_at: row.get(5)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        // Update session cursor if we got events
        let new_cursor = events.last().map(|e| e.id);
        if let (Some(sid), Some(nc)) = (session_id, new_cursor) {
            conn.execute(
                "UPDATE sessions SET cursor = ?1 WHERE id = ?2",
                params![nc, sid],
            )?;
        }

        Ok((events, new_cursor))
    }

    /// Get a specific session by ID.
    pub fn get_session(&self, session_id: &str) -> Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();

        let session = conn.query_row(
            "SELECT id, name, machine, cwd, client_id, cursor, last_heartbeat, created_at
             FROM sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok(Session {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    machine: row.get(2)?,
                    cwd: row.get(3)?,
                    client_id: row.get(4)?,
                    cursor: row.get(5)?,
                    last_heartbeat: row.get(6)?,
                    created_at: row.get(7)?,
                })
            },
        );

        match session {
            Ok(s) => Ok(Some(s)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Prune old events (keep last N days).
    pub fn prune_events(&self, days: i64) -> Result<usize> {
        let conn = self.conn.lock().unwrap();
        let cutoff = Self::now() - (days * 24 * 60 * 60);
        let rows = conn.execute("DELETE FROM events WHERE created_at <= ?1", params![cutoff])?;
        Ok(rows)
    }
}

/// Generate a UUID v4 (random).
fn uuid_v4() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut bytes: [u8; 16] = rng.r#gen();

    // Set version (4) in byte 6 high nibble
    bytes[6] = (bytes[6] & 0x0f) | 0x40;

    // Set variant (10xx) in byte 8 high bits
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// Format Unix timestamp as ISO 8601.
pub fn format_timestamp(ts: i64) -> String {
    use chrono::{DateTime, Utc};
    let dt = DateTime::<Utc>::from_timestamp(ts, 0)
        .unwrap_or_else(|| DateTime::<Utc>::from_timestamp(0, 0).unwrap());
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_session() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus
            .register_session("test-branch", Some("localhost"), Some("/tmp"), None)
            .unwrap();

        assert!(!session.id.is_empty());
        assert_eq!(session.name, "test-branch");
        assert_eq!(session.machine, "localhost");
        assert_eq!(session.cwd, "/tmp");
    }

    #[test]
    fn test_session_resume() {
        let bus = EventBus::open_in_memory().unwrap();

        // Register with client_id
        let session1 = bus
            .register_session("branch1", Some("host1"), Some("/a"), Some("client-123"))
            .unwrap();

        // Register again with same machine/client_id - should resume
        let session2 = bus
            .register_session("branch2", Some("host1"), Some("/b"), Some("client-123"))
            .unwrap();

        assert_eq!(session1.id, session2.id);
        assert_eq!(session2.name, "branch2"); // Name updated
        assert_eq!(session2.cwd, "/b"); // CWD updated
    }

    #[test]
    fn test_list_sessions() {
        let bus = EventBus::open_in_memory().unwrap();

        bus.register_session("session1", Some("host1"), Some("/a"), None)
            .unwrap();
        bus.register_session("session2", Some("host2"), Some("/b"), None)
            .unwrap();

        let sessions = bus.list_sessions().unwrap();
        assert_eq!(sessions.len(), 2);
    }

    #[test]
    fn test_unregister_session() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus.register_session("test", None, None, None).unwrap();

        assert!(bus.unregister_session(Some(&session.id), None).unwrap());

        let sessions = bus.list_sessions().unwrap();
        assert!(sessions.is_empty());
    }

    #[test]
    fn test_publish_and_get_events() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus.register_session("test", None, None, None).unwrap();

        bus.publish_event("task_completed", "Done!", Some(&session.id), "all")
            .unwrap();
        bus.publish_event("ci_passed", "All green", Some(&session.id), "repo:clemini")
            .unwrap();

        let (events, _) = bus
            .get_events(None, 10, Some(&session.id), "desc", None, false, None)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "ci_passed"); // desc order
    }

    #[test]
    fn test_channel_filtering() {
        let bus = EventBus::open_in_memory().unwrap();

        bus.publish_event("e1", "p1", None, "all").unwrap();
        bus.publish_event("e2", "p2", None, "repo:foo").unwrap();
        bus.publish_event("e3", "p3", None, "repo:bar").unwrap();

        let (events, _) = bus
            .get_events(None, 10, None, "asc", Some("repo:foo"), false, None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "e2");
    }

    #[test]
    fn test_event_type_filtering() {
        let bus = EventBus::open_in_memory().unwrap();

        bus.publish_event("task_completed", "p1", None, "all")
            .unwrap();
        bus.publish_event("ci_passed", "p2", None, "all").unwrap();
        bus.publish_event("task_completed", "p3", None, "all")
            .unwrap();

        let types = vec!["task_completed".to_string()];
        let (events, _) = bus
            .get_events(None, 10, None, "asc", None, false, Some(&types))
            .unwrap();
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn test_cursor_tracking() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus.register_session("test", None, None, None).unwrap();

        bus.publish_event("e1", "p1", None, "all").unwrap();
        bus.publish_event("e2", "p2", None, "all").unwrap();
        bus.publish_event("e3", "p3", None, "all").unwrap();

        // Get first 2
        let (events, cursor) = bus
            .get_events(None, 2, Some(&session.id), "asc", None, false, None)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(cursor.is_some());

        // Get remaining with cursor
        let (events, _) = bus
            .get_events(cursor, 10, Some(&session.id), "asc", None, false, None)
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "e3");
    }

    #[test]
    fn test_resume_cursor() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus.register_session("test", None, None, None).unwrap();

        bus.publish_event("e1", "p1", None, "all").unwrap();
        bus.publish_event("e2", "p2", None, "all").unwrap();

        // Get events (cursor saved)
        let _ = bus
            .get_events(None, 1, Some(&session.id), "asc", None, false, None)
            .unwrap();

        // Add more events
        bus.publish_event("e3", "p3", None, "all").unwrap();

        // Resume from saved cursor
        let (events, _) = bus
            .get_events(None, 10, Some(&session.id), "asc", None, true, None)
            .unwrap();
        assert_eq!(events.len(), 2); // e2 and e3
    }

    #[test]
    fn test_uuid_v4_format() {
        let id = uuid_v4();
        assert_eq!(id.len(), 36);
        assert!(id.chars().nth(8) == Some('-'));
        assert!(id.chars().nth(13) == Some('-'));
        assert!(id.chars().nth(14) == Some('4')); // Version 4
    }

    #[test]
    fn test_heartbeat() {
        let bus = EventBus::open_in_memory().unwrap();
        let session = bus.register_session("test", None, None, None).unwrap();

        assert!(bus.heartbeat(&session.id).unwrap());
        assert!(!bus.heartbeat("nonexistent").unwrap());
    }

    #[test]
    fn test_prune_events() {
        let bus = EventBus::open_in_memory().unwrap();

        bus.publish_event("e1", "p1", None, "all").unwrap();

        // Prune events older than 0 days (all of them)
        let pruned = bus.prune_events(0).unwrap();
        assert_eq!(pruned, 1);

        let (events, _) = bus
            .get_events(None, 10, None, "asc", None, false, None)
            .unwrap();
        assert!(events.is_empty());
    }
}
