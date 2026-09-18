//! Read-only SQLite opens that never create a file.
//!
//! A plain `SQLITE_OPEN_READ_ONLY` open of a WAL database creates the `-shm`
//! and `-wal` side files when they are missing, and leaves them behind. The
//! atlas reads other projects' indexes (whose `.codegraph/` it must never
//! write) and its own store from readers that must not create anything, so:
//! when both side files exist — a writer is live, or left them — open
//! read-only as usual (nothing new is created); otherwise no WAL writer is
//! active, the main file holds every committed page, and it is opened
//! `immutable` (no locks, no side files).

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// Open `db` for reading without creating any file next to it.
pub(crate) fn open_read_only(db: &Path, busy: Duration) -> rusqlite::Result<Connection> {
    let wal = side_file(db, "-wal");
    let shm = side_file(db, "-shm");
    let immutable = !(wal.exists() && shm.exists());
    let uri = format!(
        "file:{}?mode=ro{}",
        encode_path(db),
        if immutable { "&immutable=1" } else { "" }
    );
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(uri, flags)?;
    conn.busy_timeout(busy)?;
    Ok(conn)
}

fn side_file(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Percent-encode a filesystem path for a `file:` URI (RFC 3986 path
/// characters and `/` pass through).
fn encode_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric() || b"/-._~!$&'()*+,;=:@".contains(&byte);
        if keep {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn reading_a_closed_wal_database_creates_no_side_files() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("with space #1");
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("t.db");
        {
            let conn = Connection::open(&db).unwrap();
            conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
                .unwrap();
            conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (7);")
                .unwrap();
        }
        assert_eq!(entries(&dir), ["t.db"]);
        let conn = open_read_only(&db, Duration::from_millis(10)).unwrap();
        let x: i64 = conn.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, 7);
        assert!(conn.execute("INSERT INTO t VALUES (8)", []).is_err());
        drop(conn);
        assert_eq!(entries(&dir), ["t.db"]);
    }

    #[test]
    fn reading_beside_a_live_writer_sees_its_commits() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("t.db");
        let writer = Connection::open(&db).unwrap();
        writer
            .query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
            .unwrap();
        writer
            .execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
            .unwrap();
        let reader = open_read_only(&db, Duration::from_millis(10)).unwrap();
        let n: i64 = reader
            .query_row("SELECT COUNT(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
