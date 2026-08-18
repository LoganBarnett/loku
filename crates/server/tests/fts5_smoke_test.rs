// Integration tests under tests/ may use the panicking variants (unwrap,
// expect, panic) freely — see llms.org's "No unwrap or expect" test exemption.
// clippy's is_in_test heuristic does not recognize tests/ integration tests as
// test code, so the workspace-level denials reach them and must be allowed at
// the file level.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]

//! Hard gate for the bundled-SQLite FTS5 requirement.  The media index
//! depends on FTS5 being compiled into rusqlite's bundled SQLite; failing
//! here, before any index code exists, points directly at the dependency
//! configuration rather than at index internals.

use rusqlite::Connection;

#[test]
fn bundled_sqlite_has_fts5() {
  let conn = Connection::open_in_memory().unwrap();
  conn
    .execute_batch(
      "CREATE VIRTUAL TABLE t USING fts5(content);
       INSERT INTO t (content) VALUES ('hello world');",
    )
    .unwrap();
  let hits: i64 = conn
    .query_row("SELECT count(*) FROM t WHERE t MATCH 'hello'", [], |row| {
      row.get(0)
    })
    .unwrap();
  assert_eq!(hits, 1);
}
