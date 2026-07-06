CREATE TABLE IF NOT EXISTS users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    lic_id BLOB,
    lic_data BLOB,
    hostname TEXT,
    first_seen INTEGER DEFAULT (strftime('%s', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS user_rec ON users (lic_id, lic_data, hostname);
CREATE UNIQUE INDEX IF NOT EXISTS user_hn_null ON users (lic_id, lic_data) WHERE hostname IS NULL;

CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    chksum BLOB UNIQUE
);

CREATE TABLE IF NOT EXISTS dbs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    file_path TEXT,
    idb_path TEXT,
    file_id INTEGER REFERENCES files (id),
    user_id INTEGER REFERENCES users (id)
);
CREATE UNIQUE INDEX IF NOT EXISTS db_paths ON dbs (file_id, user_id, idb_path);

CREATE TABLE IF NOT EXISTS funcs (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    len INTEGER NOT NULL,
    db_id INTEGER NOT NULL REFERENCES dbs (id),
    chksum BLOB,
    metadata BLOB,
    rank INTEGER,
    push_dt INTEGER DEFAULT (strftime('%s', 'now')),
    update_dt INTEGER DEFAULT (strftime('%s', 'now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS funcs_db ON funcs (chksum, db_id);
CREATE INDEX IF NOT EXISTS funcs_ranking ON funcs (chksum, rank);
CREATE INDEX IF NOT EXISTS func_chksum ON funcs (chksum);
