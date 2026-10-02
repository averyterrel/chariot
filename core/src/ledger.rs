use std::{collections::HashSet, path::Path, sync::Mutex, time::Duration};

use rusqlite::{Connection, params};

pub struct Ledger(Mutex<Connection>);

impl Ledger {
    pub fn get(path: impl AsRef<Path>) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;

        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS ledger (
                category TEXT NOT NULL,
                hash BLOB NOT NULL,
                effective_hash BLOB NOT NULL,
                PRIMARY KEY(category, hash)
            ) STRICT;
            ",
        )?;

        Ok(Self(Mutex::new(conn)))
    }

    pub fn lookup(&self, category: &str, hash: u128) -> Result<Option<u128>, rusqlite::Error> {
        match self.0.lock().unwrap().query_one(
            "SELECT effective_hash FROM ledger WHERE category = ? AND hash = ?",
            params![category, hash.to_be_bytes()],
            |row| Ok(u128::from_be_bytes(row.get::<usize, [u8; 16]>(0)?)),
        ) {
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err),
            Ok(hash) => Ok(Some(hash)),
        }
    }

    pub fn record(&self, category: &str, hash: u128, effective_hash: u128) -> Result<(), rusqlite::Error> {
        self.0.lock().unwrap().execute(
            "REPLACE INTO ledger (category, hash, effective_hash) VALUES (?, ?, ?)",
            params![category, hash.to_be_bytes(), effective_hash.to_be_bytes()],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<(String, u128, u128)>, rusqlite::Error> {
        let conn = self.0.lock().unwrap();
        let mut stmt = conn.prepare("SELECT category, hash, effective_hash FROM ledger ORDER BY category, hash")?;
        let entries = stmt.query_map([], |row| {
            Ok((
                row.get::<usize, String>(0)?,
                u128::from_be_bytes(row.get::<usize, [u8; 16]>(1)?),
                u128::from_be_bytes(row.get::<usize, [u8; 16]>(2)?),
            ))
        })?;

        entries.collect()
    }

    pub fn prune(&self, exclude: HashSet<(&str, u128)>) -> Result<(), rusqlite::Error> {
        let conn = self.0.lock().unwrap();
        let tx = conn.unchecked_transaction()?;

        let mut stmt = tx.prepare("SELECT category, hash FROM ledger")?;
        let all_records = stmt.query_map([], |row| {
            Ok((row.get::<usize, String>(0)?, u128::from_be_bytes(row.get::<usize, [u8; 16]>(1)?)))
        })?;

        for record in all_records {
            let (category, hash) = record?;

            if exclude.contains(&(category.as_str(), hash)) {
                continue;
            }

            let rows_changed = tx.execute(
                "DELETE FROM ledger WHERE category = ? AND hash = ?",
                params![category, hash.to_be_bytes()],
            )?;
            assert!(rows_changed == 1);
        }

        drop(stmt);

        tx.commit()
    }
}
