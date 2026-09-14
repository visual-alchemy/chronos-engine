use std::path::Path;

use rusqlite::{Connection, OptionalExtension, Result};

use crate::domain::MediaProbe;

pub struct Database {
    connection: Connection,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self> {
        let database = Self {
            connection: Connection::open(path)?,
        };
        database.migrate()?;
        Ok(database)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let database = Self {
            connection: Connection::open_in_memory()?,
        };
        database.migrate()?;
        Ok(database)
    }

    fn migrate(&self) -> Result<()> {
        self.connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS media_cache (
                path TEXT PRIMARY KEY,
                probe_json TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            CREATE TABLE IF NOT EXISTS profiles (
                id TEXT PRIMARY KEY,
                definition_json TEXT NOT NULL
            );
            DROP TABLE IF EXISTS streams;
            CREATE TABLE streams (
                id TEXT PRIMARY KEY,
                udp_port INTEGER NOT NULL UNIQUE CHECK (udp_port BETWEEN 10000 AND 10049),
                config_json TEXT NOT NULL DEFAULT '{}',
                state TEXT NOT NULL DEFAULT 'draft'
            );
            CREATE TABLE IF NOT EXISTS stream_history (
                id INTEGER PRIMARY KEY,
                stream_id TEXT NOT NULL,
                state TEXT NOT NULL,
                detail TEXT,
                created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
            );",
        )
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn allocate_port(&self, stream_id: &str, port: u16) -> Result<()> {
        self.connection.execute(
            "INSERT INTO streams (id, udp_port) VALUES (?1, ?2)",
            (stream_id, port),
        )?;
        Ok(())
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn release_port(&self, stream_id: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM streams WHERE id = ?1", [stream_id])?;
        Ok(())
    }

    pub fn release_runtime_allocations(&self) -> Result<()> {
        self.connection.execute("DELETE FROM streams", [])?;
        Ok(())
    }

    pub fn cache_probe(&self, path: &Path, probe: &MediaProbe) -> Result<()> {
        let probe_json = serde_json::to_string(probe)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.connection.execute(
            "INSERT INTO media_cache (path, probe_json, updated_at) VALUES (?1, ?2, CURRENT_TIMESTAMP)
             ON CONFLICT(path) DO UPDATE SET probe_json = excluded.probe_json, updated_at = excluded.updated_at",
            (path.to_string_lossy().as_ref(), probe_json.as_str()),
        )?;
        Ok(())
    }

    pub fn cached_probe(&self, path: &Path) -> Result<Option<MediaProbe>> {
        let value: Option<String> = self
            .connection
            .query_row(
                "SELECT probe_json FROM media_cache WHERE path = ?1",
                [path.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .optional()?;
        value
            .map(|json| {
                serde_json::from_str(&json).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()
    }

    pub fn save_profile(&self, id: &str, profile: &crate::profiles::OutputProfile) -> Result<()> {
        let json = serde_json::to_string(profile)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        self.connection.execute("INSERT INTO profiles (id, definition_json) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET definition_json = excluded.definition_json", (id, json))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Database;

    #[test]
    fn allocates_each_udp_port_only_once() {
        let database = Database::open_in_memory().expect("database");

        database
            .allocate_port("stream-a", 10000)
            .expect("first allocation");
        assert!(database.allocate_port("stream-b", 10000).is_err());
    }

    #[test]
    fn releases_a_port_for_another_stream() {
        let database = Database::open_in_memory().expect("database");
        database
            .allocate_port("stream-a", 10000)
            .expect("allocation");
        database.release_port("stream-a").expect("release");
        database
            .allocate_port("stream-b", 10000)
            .expect("reallocation");
    }

    #[test]
    fn releases_non_durable_allocations_on_restart() {
        let database = Database::open_in_memory().expect("database");
        database
            .allocate_port("stream-a", 10000)
            .expect("allocation");
        database
            .release_runtime_allocations()
            .expect("release allocations");
        database
            .allocate_port("stream-b", 10000)
            .expect("reallocation");
    }

    #[test]
    fn migrates_a_stale_port_check_constraint() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("chronos.db");

        {
            let connection = rusqlite::Connection::open(&path).expect("open");
            connection
                .execute_batch(
                    "CREATE TABLE streams (
                        id TEXT PRIMARY KEY,
                        udp_port INTEGER NOT NULL UNIQUE CHECK (udp_port BETWEEN 9000 AND 9099),
                        config_json TEXT NOT NULL DEFAULT '{}',
                        state TEXT NOT NULL DEFAULT 'draft'
                    );",
                )
                .expect("create legacy schema");
        }

        let database = Database::open(&path).expect("reopen");

        database
            .allocate_port("stream-a", 10000)
            .expect("in-range port after migration");
        assert!(database.allocate_port("stream-b", 9000).is_err());
    }
}
