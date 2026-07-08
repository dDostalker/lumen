use log::*;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use time::OffsetDateTime;
use turso::{Builder, Connection, Value, params_from_iter};

pub mod schema;

/// Maximum number of host parameters per statement. SQLite-compatible engines
/// usually cap this around 999 / 32766. We pick a conservative value that fits
/// the per-row column count used in bulk inserts.
const SQLITE_MAX_VARS: usize = 900;

pub struct Database {
    conn: Connection,
}

pub struct FunctionInfo {
    pub name: String,
    pub len: u32,
    pub data: Vec<u8>,
    pub popularity: u32,
}

#[derive(Debug, Serialize)]
pub struct DbStats {
    unique_lics: i32,
    unique_hosts_per_lic: i32,

    unique_funcs: i32,
    total_funcs: i32,

    dbs: i32,
    unique_files: i32,
}

const SCHEMA_SQL: &str = include_str!("schema.sql");

impl Database {
    pub async fn open(config: &crate::config::Database) -> Result<Self, anyhow::Error> {
        // Make sure the parent directory exists so we can create the file.
        if let Some(parent) = config.path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        let path = config.path.to_string_lossy().into_owned();
        info!("opening turso database at {:?}", config.path);
        let db = Builder::new_local(&path).build().await?;
        let conn = db.connect()?;

        // Apply the schema. This is idempotent (`CREATE TABLE IF NOT EXISTS`).
        conn.execute_batch(SCHEMA_SQL).await?;

        // Enable WAL for better concurrency on supporting builds.
        let _ = conn.pragma_update("journal_mode", "WAL").await;

        Ok(Database { conn })
    }

    pub async fn get_funcs(
        &self, funcs: &[crate::rpc::PullMetadataFunc<'_>],
    ) -> Result<Vec<Option<FunctionInfo>>, anyhow::Error> {
        let chksums: Vec<&[u8]> = funcs.iter().map(|v| v.mb_hash).collect();

        let mut partial: HashMap<Vec<u8>, FunctionInfo> = HashMap::new();

        // SQLite has no `ANY($array)`; emulate by chunking `IN (?, ?, ...)` queries.
        for chunk in chksums.chunks(SQLITE_MAX_VARS) {
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "WITH best AS (
                    SELECT chksum, MAX(rank) AS maxrank FROM funcs
                    WHERE chksum IN ({placeholders})
                    GROUP BY chksum
                )
                SELECT f2.name, f2.len, f2.metadata, f2.chksum
                FROM best
                LEFT JOIN funcs f2 ON (best.chksum = f2.chksum AND best.maxrank = f2.rank)"
            );

            let params: Vec<Value> = chunk.iter().map(|c| Value::Blob(c.to_vec())).collect();
            let mut rows = self.conn.query(&sql, params_from_iter(params)).await?;

            while let Some(row) = rows.next().await? {
                let name = match row.get_value(0)? {
                    Value::Text(s) => s,
                    Value::Blob(b) => String::from_utf8_lossy(&b).into_owned(),
                    _ => String::new(),
                };
                let len = match row.get_value(1)? {
                    Value::Integer(i) => i as u32,
                    _ => 0,
                };
                let metadata = match row.get_value(2)? {
                    Value::Blob(b) => b,
                    _ => Vec::new(),
                };
                let chksum = match row.get_value(3)? {
                    Value::Blob(b) => b,
                    _ => continue,
                };

                partial.insert(chksum, FunctionInfo { name, len, data: metadata, popularity: 0 });
            }
        }

        let results = partial.len();
        let res: Vec<Option<FunctionInfo>> =
            chksums.iter().map(|&chksum| partial.remove(chksum)).collect();

        trace!("found {}/{} results", results, chksums.len());
        debug_assert_eq!(chksums.len(), res.len());
        Ok(res)
    }

    pub async fn get_or_create_user<'a>(
        &self, user: &'a crate::rpc::RpcHello<'a>, hostname: &str,
    ) -> Result<i32, anyhow::Error> {
        let lic_id = user.lic_number;
        let lic_data = user.license_data;

        let row = self
            .conn
            .query(
                "SELECT id FROM users WHERE lic_id = ? AND lic_data = ? AND hostname = ?",
                params_from_iter([
                    Value::Blob(lic_id.to_vec()),
                    Value::Blob(lic_data.to_vec()),
                    Value::Text(hostname.to_string()),
                ]),
            )
            .await?
            .next()
            .await?;

        if let Some(row) = row
            && let Value::Integer(id) = row.get_value(0)?
        {
            return Ok(id as i32);
        }

        self.conn
            .execute(
                "INSERT OR IGNORE INTO users (lic_id, lic_data, hostname) VALUES (?, ?, ?)",
                params_from_iter([
                    Value::Blob(lic_id.to_vec()),
                    Value::Blob(lic_data.to_vec()),
                    Value::Text(hostname.to_string()),
                ]),
            )
            .await?;

        let id = self
            .conn
            .query(
                "SELECT id FROM users WHERE lic_id = ? AND lic_data = ? AND hostname = ?",
                params_from_iter([
                    Value::Blob(lic_id.to_vec()),
                    Value::Blob(lic_data.to_vec()),
                    Value::Text(hostname.to_string()),
                ]),
            )
            .await?
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to create user"))?;

        let id = match id.get_value(0)? {
            Value::Integer(i) => i as i32,
            _ => anyhow::bail!("user id has unexpected type"),
        };
        Ok(id)
    }

    async fn get_or_create_file<'a>(
        &self, funcs: &'a crate::rpc::PushMetadata<'a>,
    ) -> Result<i32, anyhow::Error> {
        let hash = funcs.md5;

        let row = self
            .conn
            .query(
                "SELECT id FROM files WHERE chksum = ?",
                params_from_iter([Value::Blob(hash.to_vec())]),
            )
            .await?
            .next()
            .await?;

        if let Some(row) = row
            && let Value::Integer(id) = row.get_value(0)?
        {
            return Ok(id as i32);
        }

        self.conn
            .execute(
                "INSERT OR IGNORE INTO files (chksum) VALUES (?)",
                params_from_iter([Value::Blob(hash.to_vec())]),
            )
            .await?;

        let id = self
            .conn
            .query(
                "SELECT id FROM files WHERE chksum = ?",
                params_from_iter([Value::Blob(hash.to_vec())]),
            )
            .await?
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to create file"))?;

        let id = match id.get_value(0)? {
            Value::Integer(i) => i as i32,
            _ => anyhow::bail!("file id has unexpected type"),
        };
        Ok(id)
    }

    async fn get_or_create_db<'a>(
        &self, user: &'a crate::rpc::RpcHello<'a>, funcs: &'a crate::rpc::PushMetadata<'a>,
    ) -> Result<i32, anyhow::Error> {
        let file_id = self.get_or_create_file(funcs).await?;
        let user_id = self.get_or_create_user(user, funcs.hostname).await?;

        let params = params_from_iter([
            Value::Integer(file_id as i64),
            Value::Integer(user_id as i64),
            Value::Text(funcs.idb_path.to_string()),
        ]);

        let row = self
            .conn
            .query("SELECT id FROM dbs WHERE file_id = ? AND user_id = ? AND idb_path = ?", params)
            .await?
            .next()
            .await?;

        if let Some(row) = row
            && let Value::Integer(id) = row.get_value(0)?
        {
            return Ok(id as i32);
        }

        self.conn
            .execute(
                "INSERT OR IGNORE INTO dbs (file_id, user_id, file_path, idb_path) VALUES (?, ?, ?, ?)",
                params_from_iter(
                    [
                        Value::Integer(file_id as i64),
                        Value::Integer(user_id as i64),
                        Value::Text(funcs.file_path.to_string()),
                        Value::Text(funcs.idb_path.to_string()),
                    ]

                ),
            )
            .await?;

        let id = self
            .conn
            .query(
                "SELECT id FROM dbs WHERE file_id = ? AND user_id = ? AND idb_path = ?",
                params_from_iter([
                    Value::Integer(file_id as i64),
                    Value::Integer(user_id as i64),
                    Value::Text(funcs.idb_path.to_string()),
                ]),
            )
            .await?
            .next()
            .await?
            .ok_or_else(|| anyhow::anyhow!("failed to create db"))?;

        let id = match id.get_value(0)? {
            Value::Integer(i) => i as i32,
            _ => anyhow::bail!("db id has unexpected type"),
        };
        Ok(id)
    }

    pub async fn push_funcs<'a>(
        &self, user: &'a crate::rpc::RpcHello<'a>, funcs: &'a crate::rpc::PushMetadata<'a>,
        scores: &[u32],
    ) -> Result<Vec<bool>, anyhow::Error> {
        // Each row uses 7 bound parameters. Cap chunk size so total params < SQLITE_MAX_VARS.
        const COLS_PER_ROW: usize = 7;
        let chunk_size = (SQLITE_MAX_VARS / COLS_PER_ROW).max(1);

        let db_id = self.get_or_create_db(user, funcs).await?;

        let mut is_new = Vec::with_capacity(funcs.funcs.len());

        for chunk in funcs.funcs.chunks(chunk_size).zip(scores.chunks(chunk_size)) {
            let (func_chunk, score_chunk) = chunk;

            // First, find out which chksums already exist for this db_id.
            // Sub-chunk again to stay under the parameter cap.
            let mut existing: HashSet<Vec<u8>> = HashSet::new();

            for sub in func_chunk.chunks(SQLITE_MAX_VARS) {
                let placeholders =
                    std::iter::repeat_n("?", sub.len()).collect::<Vec<_>>().join(",");
                let sql = format!(
                    "SELECT chksum FROM funcs WHERE db_id = ? AND chksum IN ({placeholders})"
                );

                let mut params: Vec<Value> = Vec::with_capacity(sub.len() + 1);
                params.push(Value::Integer(db_id as i64));
                for f in sub {
                    params.push(Value::Blob(f.hash.to_vec()));
                }

                let mut rows = self.conn.query(&sql, params_from_iter(params)).await?;
                while let Some(row) = rows.next().await? {
                    if let Value::Blob(b) = row.get_value(0)? {
                        existing.insert(b);
                    }
                }
            }

            // Build a single multi-row UPSERT.
            let mut placeholders = String::new();
            for (i, _) in func_chunk.iter().enumerate() {
                if i > 0 {
                    placeholders.push(',');
                }
                placeholders.push_str("(?, ?, ?, ?, ?, ?, ?)");
            }

            let sql = format!(
                "INSERT INTO funcs (name, len, chksum, metadata, rank, db_id, update_dt)
                 VALUES {placeholders}
                 ON CONFLICT(chksum, db_id) DO UPDATE SET
                     name = excluded.name,
                     metadata = excluded.metadata,
                     rank = excluded.rank,
                     update_dt = excluded.update_dt"
            );

            let now = OffsetDateTime::now_utc().unix_timestamp();
            let mut params: Vec<Value> = Vec::with_capacity(func_chunk.len() * COLS_PER_ROW);
            for (func, &score) in func_chunk.iter().zip(score_chunk.iter()) {
                params.push(Value::Text(func.name.to_string()));
                params.push(Value::Integer(func.func_len as i64));
                params.push(Value::Blob(func.hash.to_vec()));
                params.push(Value::Blob(func.func_data.to_vec()));
                params.push(Value::Integer(score as i64));
                params.push(Value::Integer(db_id as i64));
                params.push(Value::Integer(now));
            }

            self.conn.execute(&sql, params_from_iter(params)).await?;

            for func in func_chunk {
                is_new.push(!existing.contains(func.hash));
            }
        }

        Ok(is_new)
    }

    pub async fn get_file_funcs(
        &self, md5: &[u8], offset: i64, limit: i64,
    ) -> Result<Vec<(String, i32, Vec<u8>)>, anyhow::Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT f.name, f.len, f.chksum
                 FROM funcs f
                 JOIN dbs d ON d.id = f.db_id
                 JOIN files fl ON fl.id = d.file_id
                 WHERE fl.chksum = ?
                 LIMIT ? OFFSET ?",
                params_from_iter([
                    Value::Blob(md5.to_vec()),
                    Value::Integer(limit),
                    Value::Integer(offset),
                ]),
            )
            .await?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let name = match row.get_value(0)? {
                Value::Text(s) => s,
                _ => String::new(),
            };
            let len = match row.get_value(1)? {
                Value::Integer(i) => i as i32,
                _ => 0,
            };
            let chksum = match row.get_value(2)? {
                Value::Blob(b) => b,
                _ => Vec::new(),
            };
            out.push((name, len, chksum));
        }
        Ok(out)
    }

    pub async fn get_files_with_func(&self, func: &[u8]) -> Result<Vec<Vec<u8>>, anyhow::Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT DISTINCT fl.chksum
                 FROM files fl
                 JOIN dbs d ON d.file_id = fl.id
                 JOIN funcs f ON f.db_id = d.id
                 WHERE f.chksum = ?",
                params_from_iter([Value::Blob(func.to_vec())]),
            )
            .await?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            if let Value::Blob(b) = row.get_value(0)? {
                out.push(b);
            }
        }
        Ok(out)
    }

    pub async fn delete_metadata(
        &self, req: &crate::rpc::DelHistory<'_>,
    ) -> Result<(), anyhow::Error> {
        let mut total = 0u64;
        for chunk in req.funcs.chunks(SQLITE_MAX_VARS) {
            let placeholders = std::iter::repeat_n("?", chunk.len()).collect::<Vec<_>>().join(",");
            let sql = format!("DELETE FROM funcs WHERE chksum IN ({placeholders})");

            let params: Vec<Value> = chunk.iter().map(|c| Value::Blob(c.to_vec())).collect();
            total += self.conn.execute(&sql, params_from_iter(params)).await?;
        }

        debug!("deleted {total} rows");
        Ok(())
    }

    pub async fn get_func_histories(
        &self, chksum: &[u8], limit: u32,
    ) -> Result<Vec<(OffsetDateTime, String, Vec<u8>)>, anyhow::Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT update_dt, name, metadata FROM funcs
                 WHERE chksum = ? AND update_dt IS NOT NULL
                 ORDER BY update_dt DESC
                 LIMIT ?",
                params_from_iter([Value::Blob(chksum.to_vec()), Value::Integer(limit as i64)]),
            )
            .await?;

        let mut out = Vec::new();
        while let Some(row) = rows.next().await? {
            let ts = match row.get_value(0)? {
                Value::Integer(i) => i,
                _ => 0,
            };
            let name = match row.get_value(1)? {
                Value::Text(s) => s,
                _ => String::new(),
            };
            let metadata = match row.get_value(2)? {
                Value::Blob(b) => b,
                _ => Vec::new(),
            };
            let dt = OffsetDateTime::from_unix_timestamp(ts).unwrap_or(OffsetDateTime::UNIX_EPOCH);
            out.push((dt, name, metadata));
        }
        Ok(out)
    }

    /// Verify a username/password against the web_users table.
    /// Returns true if the credentials are valid.
    pub async fn verify_web_user(
        &self, username: &str, password: &str,
    ) -> Result<bool, anyhow::Error> {
        let mut rows = self
            .conn
            .query(
                "SELECT password_hash FROM web_users WHERE username = ?",
                params_from_iter([Value::Text(username.to_string())]),
            )
            .await?;

        let row = rows.next().await?;
        let stored_hash = match row {
            Some(row) => match row.get_value(0)? {
                Value::Text(s) => s,
                _ => return Ok(false),
            },
            None => return Ok(false),
        };

        // Format: "salt:hash"
        let parts: Vec<&str> = stored_hash.split(':').collect();
        if parts.len() != 2 {
            return Ok(false);
        }
        let salt = parts[0];
        let expected_hash = parts[1];

        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(password.as_bytes());
        let computed = hex::encode(hasher.finalize());

        Ok(computed == expected_hash)
    }

    /// Hash a password with a random salt. Returns "salt:hash" format.
    pub fn hash_password(password: &str) -> String {
        use std::time::{SystemTime, UNIX_EPOCH};
        let salt =
            format!("{:x}", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos());
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(password.as_bytes());
        let hash = hex::encode(hasher.finalize());
        format!("{salt}:{hash}")
    }

    /// Create or update a web user with a hashed password.
    /// If the user already exists, the password is updated.
    pub async fn upsert_web_user(
        &self, username: &str, password: &str,
    ) -> Result<(), anyhow::Error> {
        let password_hash = Self::hash_password(password);
        self.conn
            .execute(
                "INSERT INTO web_users (username, password_hash) VALUES (?, ?)
                 ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash",
                params_from_iter([Value::Text(username.to_string()), Value::Text(password_hash)]),
            )
            .await?;
        Ok(())
    }

    /// Check if any web users exist.
    pub async fn has_web_users(&self) -> Result<bool, anyhow::Error> {
        let params: Vec<Value> = vec![];
        let mut rows =
            self.conn.query("SELECT COUNT(*) FROM web_users", params_from_iter(params)).await?;
        if let Some(row) = rows.next().await?
            && let Value::Integer(count) = row.get_value(0)?
        {
            return Ok(count > 0);
        }
        Ok(false)
    }
}
