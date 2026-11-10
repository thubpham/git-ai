use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use rusqlite::{Connection, OptionalExtension, Transaction, named_params, params};

use super::claude_facts::{CallFact, TranscriptFact, TurnFact};
use super::db::{run_migrations, to_db_i64};
use super::spans::{EditType, PriorKind, TouchKind};
use crate::error::GitAiError;

const MIGRATIONS: &[&str] = &[
    r#"
    CREATE TABLE IF NOT EXISTS schema_version (
        version INTEGER PRIMARY KEY
    );

    CREATE TABLE call (
        request_id TEXT PRIMARY KEY,
        message_id TEXT,
        uuid TEXT,
        parent_uuid TEXT,
        prompt_id TEXT,
        model TEXT,
        advisor_model TEXT,
        effort TEXT,
        turn_effort TEXT,
        speed TEXT,
        stop_reason TEXT,
        input_tokens INTEGER,
        output_tokens INTEGER,
        cache_read_tokens INTEGER,
        cache_write_tokens INTEGER,
        cache_write_5m_tokens INTEGER,
        cache_write_1h_tokens INTEGER,
        thinking_tokens INTEGER,
        iterations_json TEXT,
        timestamp INTEGER,
        thinking_ms INTEGER
    );

    CREATE TABLE call_tool_use (
        tool_use_id TEXT PRIMARY KEY,
        request_id TEXT NOT NULL
    );
    CREATE INDEX idx_call_tool_use_request ON call_tool_use(request_id);

    CREATE TABLE advisor_use (
        request_id TEXT NOT NULL,
        advisor_index INTEGER NOT NULL,
        model TEXT NOT NULL,
        input_tokens INTEGER NOT NULL,
        output_tokens INTEGER NOT NULL,
        cache_read_tokens INTEGER NOT NULL,
        cache_write_tokens INTEGER NOT NULL,
        PRIMARY KEY (request_id, advisor_index)
    );

    CREATE TABLE turn (
        prompt_id TEXT PRIMARY KEY,
        duration_ms INTEGER,
        message_count INTEGER,
        timestamp INTEGER
    );

    INSERT INTO schema_version (version) VALUES (1);
    "#,
    r#"
    CREATE TABLE span (
        span_id TEXT PRIMARY KEY,
        session_id TEXT NOT NULL,
        tool_use_id TEXT,
        prompt_id TEXT,
        request_id TEXT,
        repo_key TEXT NOT NULL,
        file_path TEXT NOT NULL,
        author_kind TEXT NOT NULL,
        author_id TEXT NOT NULL,
        source TEXT NOT NULL,
        content_hash TEXT NOT NULL,
        created_at INTEGER NOT NULL,
        link_status TEXT NOT NULL DEFAULT 'pending',
        link_attempts INTEGER NOT NULL DEFAULT 0,
        next_link_at INTEGER,
        pinned_commit TEXT,
        pinned_blob TEXT,
        pinned_at INTEGER
    );
    CREATE INDEX idx_span_tool_use ON span(session_id, tool_use_id);
    CREATE INDEX idx_span_pending_link ON span(next_link_at) WHERE link_status = 'pending';
    CREATE INDEX idx_span_repo_file ON span(repo_key, file_path);

    CREATE TABLE span_piece (
        span_id TEXT NOT NULL,
        piece_index INTEGER NOT NULL,
        start_byte INTEGER NOT NULL,
        end_byte INTEGER NOT NULL,
        PRIMARY KEY (span_id, piece_index)
    );

    CREATE TABLE survival_measurement (
        span_id TEXT NOT NULL,
        measured_at INTEGER NOT NULL,
        committed_at INTEGER NOT NULL,
        surviving_bytes INTEGER,
        total_bytes INTEGER NOT NULL,
        outcome TEXT NOT NULL,
        PRIMARY KEY (span_id, measured_at)
    );

    INSERT INTO schema_version (version) VALUES (2);
    "#,
    r#"
    ALTER TABLE span ADD COLUMN prior_kind TEXT
        CHECK (prior_kind IN ('ai', 'known_human', 'untracked', 'mixed', 'none'));
    ALTER TABLE span ADD COLUMN edit_type TEXT
        CHECK (edit_type IN ('add', 'replace'));

    CREATE TABLE span_touch (
        touch_id INTEGER PRIMARY KEY,
        span_id TEXT NOT NULL,
        touched_at INTEGER NOT NULL,
        toucher_kind TEXT NOT NULL,
        toucher_id TEXT NOT NULL,
        touch_kind TEXT NOT NULL CHECK (touch_kind IN ('replace', 'delete')),
        bytes_removed INTEGER NOT NULL
    );
    CREATE INDEX idx_span_touch_span ON span_touch(span_id);

    INSERT INTO schema_version (version) VALUES (3);
    "#,
    r#"
    ALTER TABLE span ADD COLUMN total_bytes INTEGER;

    INSERT INTO schema_version (version) VALUES (4);
    "#,
];

fn insert_pieces(
    tx: &Transaction,
    span_id: &str,
    pieces: &[(usize, usize)],
) -> Result<(), GitAiError> {
    for (index, (start, end)) in pieces.iter().enumerate() {
        tx.execute(
            "INSERT INTO span_piece (span_id, piece_index, start_byte, end_byte)
             VALUES (?1, ?2, ?3, ?4)",
            params![span_id, index as i64, *start as i64, *end as i64],
        )?;
    }
    Ok(())
}

pub const LINK_DEADLINE_SECS: i64 = 30 * 60;

const LINK_PENDING_SPANS_SQL: &str = "
    UPDATE span
    SET request_id = (
            SELECT request_id FROM call_tool_use
            WHERE call_tool_use.tool_use_id = span.tool_use_id
        ),
        link_status = 'linked',
        next_link_at = NULL
    WHERE link_status = 'pending'
      AND tool_use_id IN (SELECT tool_use_id FROM call_tool_use)";

const CALL_UPSERT_SQL: &str = "
    INSERT INTO call (
        request_id, message_id, uuid, parent_uuid, prompt_id, model, advisor_model, effort,
        turn_effort, speed, stop_reason, input_tokens, output_tokens, cache_read_tokens,
        cache_write_tokens, cache_write_5m_tokens, cache_write_1h_tokens, thinking_tokens,
        iterations_json, timestamp, thinking_ms
    ) VALUES (
        :request_id, :message_id, :uuid, :parent_uuid, :prompt_id, :model, :advisor_model, :effort,
        :turn_effort, :speed, :stop_reason, :input_tokens, :output_tokens, :cache_read_tokens,
        :cache_write_tokens, :cache_write_5m_tokens, :cache_write_1h_tokens, :thinking_tokens,
        :iterations_json, :timestamp, :thinking_ms
    )
    ON CONFLICT(request_id) DO UPDATE SET
        message_id = COALESCE(call.message_id, excluded.message_id),
        uuid = COALESCE(call.uuid, excluded.uuid),
        parent_uuid = COALESCE(call.parent_uuid, excluded.parent_uuid),
        prompt_id = COALESCE(call.prompt_id, excluded.prompt_id),
        model = COALESCE(call.model, excluded.model),
        advisor_model = COALESCE(call.advisor_model, excluded.advisor_model),
        effort = COALESCE(call.effort, excluded.effort),
        turn_effort = COALESCE(call.turn_effort, excluded.turn_effort),
        speed = COALESCE(call.speed, excluded.speed),
        stop_reason = COALESCE(call.stop_reason, excluded.stop_reason),
        input_tokens = COALESCE(call.input_tokens, excluded.input_tokens),
        output_tokens = COALESCE(call.output_tokens, excluded.output_tokens),
        cache_read_tokens = COALESCE(call.cache_read_tokens, excluded.cache_read_tokens),
        cache_write_tokens = COALESCE(call.cache_write_tokens, excluded.cache_write_tokens),
        cache_write_5m_tokens = COALESCE(call.cache_write_5m_tokens, excluded.cache_write_5m_tokens),
        cache_write_1h_tokens = COALESCE(call.cache_write_1h_tokens, excluded.cache_write_1h_tokens),
        thinking_tokens = COALESCE(call.thinking_tokens, excluded.thinking_tokens),
        iterations_json = COALESCE(call.iterations_json, excluded.iterations_json),
        timestamp = COALESCE(call.timestamp, excluded.timestamp),
        thinking_ms = COALESCE(call.thinking_ms, excluded.thinking_ms)";

static SHARED: OnceLock<Arc<FactsDatabase>> = OnceLock::new();

pub fn register_shared(db: Arc<FactsDatabase>) {
    let _ = SHARED.set(db);
}

pub fn shared() -> Option<Arc<FactsDatabase>> {
    SHARED.get().cloned()
}

pub struct NewSpan {
    pub span_id: String,
    pub session_id: String,
    pub tool_use_id: Option<String>,
    pub prompt_id: Option<String>,
    pub repo_key: String,
    pub file_path: String,
    pub author_kind: String,
    pub author_id: String,
    pub source: String,
    pub content_hash: String,
    pub created_at: i64,
    pub prior_kind: PriorKind,
    pub edit_type: EditType,
    pub pieces: Vec<(usize, usize)>,
}

pub struct LiveSpan {
    pub span_id: String,
    pub author_id: String,
    pub pieces: Vec<(usize, usize)>,
}

pub struct SpanTouch {
    pub touched_at: i64,
    pub toucher_kind: String,
    pub toucher_id: String,
    pub touch_kind: TouchKind,
    pub bytes_removed: usize,
}

pub struct SpanRemap {
    pub span_id: String,
    pub pieces: Vec<(usize, usize)>,
    pub touch: Option<SpanTouch>,
}

pub struct SpanSurvival {
    pub span_id: String,
    pub committed_at: i64,
    pub surviving_bytes: i64,
    pub total_bytes: i64,
}

pub struct FactsDatabase {
    conn: Arc<Mutex<Connection>>,
}

impl FactsDatabase {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, GitAiError> {
        let conn = crate::sqlite::open_with_memory_limits(path.as_ref())?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "temp_store", "MEMORY")?;
        run_migrations(&conn, MIGRATIONS, "transcript-facts")?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn insert_span(&self, span: &NewSpan) -> Result<(), GitAiError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        let request_id: Option<String> = match &span.tool_use_id {
            Some(tool_use_id) => tx
                .query_row(
                    "SELECT request_id FROM call_tool_use WHERE tool_use_id = ?1",
                    params![tool_use_id],
                    |row| row.get(0),
                )
                .optional()?,
            None => None,
        };
        let (link_status, next_link_at) = match (&span.tool_use_id, &request_id) {
            (None, _) => ("none", None),
            (Some(_), Some(_)) => ("linked", None),
            (Some(_), None) => ("pending", Some(span.created_at + LINK_DEADLINE_SECS)),
        };
        tx.execute(
            "INSERT INTO span (
                span_id, session_id, tool_use_id, prompt_id, repo_key, file_path,
                author_kind, author_id, source, content_hash, created_at,
                prior_kind, edit_type, total_bytes, request_id, link_status, next_link_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                span.span_id,
                span.session_id,
                span.tool_use_id,
                span.prompt_id,
                span.repo_key,
                span.file_path,
                span.author_kind,
                span.author_id,
                span.source,
                span.content_hash,
                span.created_at,
                span.prior_kind.as_str(),
                span.edit_type.as_str(),
                span.pieces
                    .iter()
                    .map(|(start, end)| end - start)
                    .sum::<usize>() as i64,
                request_id,
                link_status,
                next_link_at,
            ],
        )?;
        insert_pieces(&tx, &span.span_id, &span.pieces)?;
        tx.commit()?;
        Ok(())
    }

    pub fn live_spans(&self, repo_key: &str, file_path: &str) -> Result<Vec<LiveSpan>, GitAiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT piece.span_id, span.author_id, piece.start_byte, piece.end_byte
             FROM span_piece piece JOIN span ON span.span_id = piece.span_id
             WHERE span.repo_key = ?1 AND span.file_path = ?2
             ORDER BY piece.span_id, piece.piece_index",
        )?;
        let rows = stmt.query_map(params![repo_key, file_path], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        let mut spans: Vec<LiveSpan> = Vec::new();
        for row in rows {
            let (span_id, author_id, start, end) = row?;
            let piece = (start as usize, end as usize);
            match spans.last_mut() {
                Some(last) if last.span_id == span_id => last.pieces.push(piece),
                _ => spans.push(LiveSpan {
                    span_id,
                    author_id,
                    pieces: vec![piece],
                }),
            }
        }
        Ok(spans)
    }

    pub fn pinned_span_survival(&self) -> Result<Vec<SpanSurvival>, GitAiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT span.span_id, span.pinned_at, span.total_bytes,
                    COALESCE(SUM(piece.end_byte - piece.start_byte), 0)
             FROM span LEFT JOIN span_piece piece ON piece.span_id = span.span_id
             WHERE span.pinned_commit IS NOT NULL
               AND span.pinned_at IS NOT NULL
               AND span.total_bytes > 0
             GROUP BY span.span_id
             ORDER BY span.pinned_at, span.span_id",
        )?;
        let survival = stmt
            .query_map([], |row| {
                Ok(SpanSurvival {
                    span_id: row.get(0)?,
                    committed_at: row.get(1)?,
                    total_bytes: row.get(2)?,
                    surviving_bytes: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(survival)
    }

    pub fn expire_unlinked_spans(&self, now: i64) -> Result<usize, GitAiError> {
        let conn = self.lock();
        let expired = conn.execute(
            "UPDATE span SET link_status = 'unknown', next_link_at = NULL
             WHERE link_status = 'pending' AND next_link_at <= ?1",
            params![now],
        )?;
        Ok(expired)
    }

    pub fn unpinned_files(&self, repo_key: &str) -> Result<HashSet<String>, GitAiError> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT DISTINCT file_path FROM span
             WHERE repo_key = ?1 AND pinned_commit IS NULL",
        )?;
        let files = stmt
            .query_map(params![repo_key], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?;
        Ok(files)
    }

    pub fn pin_spans(
        &self,
        repo_key: &str,
        commit_sha: &str,
        pinned_at: i64,
        blobs: &HashMap<String, String>,
    ) -> Result<(), GitAiError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for (file_path, blob_sha) in blobs {
            tx.execute(
                "UPDATE span SET pinned_commit = ?1, pinned_blob = ?2, pinned_at = ?3
                 WHERE repo_key = ?4 AND file_path = ?5 AND pinned_commit IS NULL",
                params![commit_sha, blob_sha, pinned_at, repo_key, file_path],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn apply_remaps(&self, remaps: &[SpanRemap]) -> Result<(), GitAiError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for remap in remaps {
            tx.execute(
                "DELETE FROM span_piece WHERE span_id = ?1",
                params![remap.span_id],
            )?;
            insert_pieces(&tx, &remap.span_id, &remap.pieces)?;
            if let Some(touch) = &remap.touch {
                tx.execute(
                    "INSERT INTO span_touch (
                        span_id, touched_at, toucher_kind, toucher_id, touch_kind, bytes_removed
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        remap.span_id,
                        touch.touched_at,
                        touch.toucher_kind,
                        touch.toucher_id,
                        touch.touch_kind.as_str(),
                        touch.bytes_removed as i64,
                    ],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn commit_facts(&self, facts: &[TranscriptFact]) -> Result<(), GitAiError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for fact in facts {
            match fact {
                TranscriptFact::Call(call) => upsert_call(&tx, call)?,
                TranscriptFact::Turn(turn) => upsert_turn(&tx, turn)?,
            }
        }
        tx.execute(LINK_PENDING_SPANS_SQL, [])?;
        tx.commit()?;
        Ok(())
    }
}

fn upsert_call(tx: &Transaction<'_>, call: &CallFact) -> Result<(), GitAiError> {
    tx.execute(
        CALL_UPSERT_SQL,
        named_params! {
            ":request_id": call.request_id,
            ":message_id": call.message_id,
            ":uuid": call.uuid,
            ":parent_uuid": call.parent_uuid,
            ":prompt_id": call.prompt_id,
            ":model": call.model,
            ":advisor_model": call.advisor_model,
            ":effort": call.effort,
            ":turn_effort": call.turn_effort,
            ":speed": call.speed,
            ":stop_reason": call.stop_reason,
            ":input_tokens": call.input_tokens.map(to_db_i64),
            ":output_tokens": call.output_tokens.map(to_db_i64),
            ":cache_read_tokens": call.cache_read_tokens.map(to_db_i64),
            ":cache_write_tokens": call.cache_write_tokens.map(to_db_i64),
            ":cache_write_5m_tokens": call.cache_write_5m_tokens.map(to_db_i64),
            ":cache_write_1h_tokens": call.cache_write_1h_tokens.map(to_db_i64),
            ":thinking_tokens": call.thinking_tokens.map(to_db_i64),
            ":iterations_json": call.iterations_json,
            ":timestamp": call.timestamp,
            ":thinking_ms": call.thinking_ms.map(to_db_i64),
        },
    )?;
    for tool_use_id in &call.tool_use_ids {
        tx.execute(
            "INSERT OR IGNORE INTO call_tool_use (tool_use_id, request_id) VALUES (?1, ?2)",
            params![tool_use_id, call.request_id],
        )?;
    }
    for advisor in &call.advisor_uses {
        tx.execute(
            "INSERT OR IGNORE INTO advisor_use (
                request_id, advisor_index, model, input_tokens, output_tokens,
                cache_read_tokens, cache_write_tokens
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                call.request_id,
                advisor.index,
                advisor.model,
                to_db_i64(advisor.input_tokens),
                to_db_i64(advisor.output_tokens),
                to_db_i64(advisor.cache_read_tokens),
                to_db_i64(advisor.cache_write_tokens),
            ],
        )?;
    }
    Ok(())
}

fn upsert_turn(tx: &Transaction<'_>, turn: &TurnFact) -> Result<(), GitAiError> {
    tx.execute(
        "INSERT INTO turn (prompt_id, duration_ms, message_count, timestamp)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(prompt_id) DO UPDATE SET
            duration_ms = COALESCE(turn.duration_ms, excluded.duration_ms),
            message_count = COALESCE(turn.message_count, excluded.message_count),
            timestamp = COALESCE(turn.timestamp, excluded.timestamp)",
        params![
            turn.prompt_id,
            turn.duration_ms.map(to_db_i64),
            turn.message_count.map(to_db_i64),
            turn.timestamp,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token_usage::claude_facts::AdvisorFact;

    fn call_of(fact: CallFact) -> TranscriptFact {
        TranscriptFact::Call(Box::new(fact))
    }

    fn db() -> (tempfile::TempDir, FactsDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let db = FactsDatabase::open(dir.path().join("transcript-facts-db")).unwrap();
        (dir, db)
    }

    #[test]
    fn the_shared_handle_is_empty_until_registered_and_keeps_the_first() {
        assert!(shared().is_none());
        let (_first_dir, first) = db();
        let first = Arc::new(first);
        register_shared(first.clone());
        let (_second_dir, second) = db();

        register_shared(Arc::new(second));

        assert!(Arc::ptr_eq(&shared().unwrap(), &first));
    }

    fn call_fact(request_id: &str) -> CallFact {
        CallFact {
            request_id: request_id.to_string(),
            ..Default::default()
        }
    }

    fn row_count(db: &FactsDatabase, table: &str) -> i64 {
        db.lock()
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn schema_version(db: &FactsDatabase) -> u32 {
        db.lock()
            .query_row(
                "SELECT version FROM schema_version ORDER BY version DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn read_call(db: &FactsDatabase, request_id: &str) -> CallFact {
        let count = |row: &rusqlite::Row<'_>, index: usize| -> rusqlite::Result<Option<u64>> {
            Ok(row.get::<_, Option<i64>>(index)?.map(|value| value as u64))
        };
        db.lock()
            .query_row(
                "SELECT message_id, uuid, parent_uuid, prompt_id, model, advisor_model, effort,
                        turn_effort, speed, stop_reason, input_tokens, output_tokens,
                        cache_read_tokens, cache_write_tokens, cache_write_5m_tokens,
                        cache_write_1h_tokens, thinking_tokens, iterations_json, timestamp,
                        thinking_ms
                 FROM call WHERE request_id = ?1",
                params![request_id],
                |row| {
                    Ok(CallFact {
                        request_id: request_id.to_string(),
                        message_id: row.get(0)?,
                        uuid: row.get(1)?,
                        parent_uuid: row.get(2)?,
                        prompt_id: row.get(3)?,
                        model: row.get(4)?,
                        advisor_model: row.get(5)?,
                        effort: row.get(6)?,
                        turn_effort: row.get(7)?,
                        speed: row.get(8)?,
                        stop_reason: row.get(9)?,
                        input_tokens: count(row, 10)?,
                        output_tokens: count(row, 11)?,
                        cache_read_tokens: count(row, 12)?,
                        cache_write_tokens: count(row, 13)?,
                        cache_write_5m_tokens: count(row, 14)?,
                        cache_write_1h_tokens: count(row, 15)?,
                        thinking_tokens: count(row, 16)?,
                        iterations_json: row.get(17)?,
                        timestamp: row.get(18)?,
                        thinking_ms: count(row, 19)?,
                        tool_use_ids: Vec::new(),
                        advisor_uses: Vec::new(),
                    })
                },
            )
            .unwrap()
    }

    #[test]
    fn migrations_apply_and_are_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-facts-db");
        let db = FactsDatabase::open(&path).unwrap();
        assert_eq!(schema_version(&db), 4);
        drop(db);
        let db = FactsDatabase::open(&path).unwrap();
        assert_eq!(schema_version(&db), 4);
    }

    fn insert_span(db: &FactsDatabase, span_id: &str) -> rusqlite::Result<usize> {
        db.lock().execute(
            "INSERT INTO span (
                span_id, session_id, repo_key, file_path, author_kind, author_id,
                source, content_hash, created_at
             ) VALUES (?1, 's1', '/repo/.git', 'src/a.rs', 'ai', 'author-1',
                       'file_edit', 'hash-1', 1000)",
            params![span_id],
        )
    }

    fn new_span(span_id: &str) -> NewSpan {
        NewSpan {
            span_id: span_id.to_string(),
            session_id: "s1".to_string(),
            tool_use_id: Some("toolu_1".to_string()),
            prompt_id: Some("p1".to_string()),
            repo_key: "/repo/.git".to_string(),
            file_path: "src/a.rs".to_string(),
            author_kind: "ai".to_string(),
            author_id: "author-1".to_string(),
            source: "file_edit".to_string(),
            content_hash: "hash-1".to_string(),
            created_at: 1000,
            prior_kind: PriorKind::KnownHuman,
            edit_type: EditType::Replace,
            pieces: vec![(0, 10), (40, 55)],
        }
    }

    #[test]
    fn inserting_a_span_writes_the_span_and_each_of_its_pieces() {
        let (_dir, db) = db();

        db.insert_span(&new_span("span-1")).unwrap();

        assert_eq!(row_count(&db, "span"), 1);
        let pieces: Vec<(i64, i64)> = {
            let conn = db.lock();
            let mut stmt = conn
                .prepare(
                    "SELECT start_byte, end_byte FROM span_piece
                     WHERE span_id = 'span-1' ORDER BY piece_index",
                )
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .map(|piece| piece.unwrap())
                .collect()
        };
        assert_eq!(pieces, vec![(0, 10), (40, 55)]);
    }

    #[test]
    fn an_inserted_span_keeps_its_join_key_and_edit_context() {
        let (_dir, db) = db();

        db.insert_span(&new_span("span-1")).unwrap();

        let stored: (String, String, String, String, String, String) = db
            .lock()
            .query_row(
                "SELECT tool_use_id, prompt_id, author_kind, prior_kind, edit_type, link_status
                 FROM span WHERE span_id = 'span-1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            stored,
            (
                "toolu_1".to_string(),
                "p1".to_string(),
                "ai".to_string(),
                "known_human".to_string(),
                "replace".to_string(),
                "pending".to_string(),
            )
        );
    }

    #[test]
    fn a_span_without_a_tool_use_id_still_saves() {
        let (_dir, db) = db();
        let mut span = new_span("span-1");
        span.tool_use_id = None;
        span.prompt_id = None;

        db.insert_span(&span).unwrap();

        let stored: (Option<String>, Option<String>) = db
            .lock()
            .query_row(
                "SELECT tool_use_id, prompt_id FROM span WHERE span_id = 'span-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (None, None));
    }

    #[test]
    fn a_duplicate_span_id_adds_no_rows() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        let mut again = new_span("span-1");
        again.pieces = vec![(0, 1), (2, 3), (4, 5)];

        assert!(db.insert_span(&again).is_err());

        assert_eq!(row_count(&db, "span"), 1);
        assert_eq!(row_count(&db, "span_piece"), 2);
    }

    #[test]
    fn a_v1_database_gains_the_span_tables_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-facts-db");
        {
            let conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute(
                "INSERT INTO call (request_id, model) VALUES ('req-1', 'main-model')",
                [],
            )
            .unwrap();
        }

        let db = FactsDatabase::open(&path).unwrap();

        assert_eq!(schema_version(&db), 4);
        assert_eq!(read_call(&db, "req-1").model.as_deref(), Some("main-model"));
        assert_eq!(row_count(&db, "span"), 0);
        assert_eq!(row_count(&db, "span_piece"), 0);
        assert_eq!(row_count(&db, "survival_measurement"), 0);
    }

    #[test]
    fn a_v2_database_gains_edit_context_and_keeps_its_spans() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-facts-db");
        {
            let conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
            conn.execute_batch(MIGRATIONS[0]).unwrap();
            conn.execute_batch(MIGRATIONS[1]).unwrap();
        }
        {
            let conn = crate::sqlite::open_with_memory_limits(&path).unwrap();
            conn.execute(
                "INSERT INTO span (
                    span_id, session_id, repo_key, file_path, author_kind, author_id,
                    source, content_hash, created_at
                 ) VALUES ('span-1', 's1', '/repo/.git', 'src/a.rs', 'ai', 'author-1',
                           'file_edit', 'hash-1', 1000)",
                [],
            )
            .unwrap();
        }

        let db = FactsDatabase::open(&path).unwrap();

        assert_eq!(schema_version(&db), 4);
        let stored: (Option<String>, Option<String>) = db
            .lock()
            .query_row(
                "SELECT prior_kind, edit_type FROM span WHERE span_id = 'span-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (None, None));
        assert_eq!(row_count(&db, "span_touch"), 0);
    }

    #[test]
    fn a_span_records_what_it_replaced_and_how() {
        let (_dir, db) = db();
        insert_span(&db, "span-1").unwrap();

        db.lock()
            .execute(
                "UPDATE span SET prior_kind = 'known_human', edit_type = 'replace'
                 WHERE span_id = 'span-1'",
                [],
            )
            .unwrap();

        let stored: (String, String) = db
            .lock()
            .query_row(
                "SELECT prior_kind, edit_type FROM span WHERE span_id = 'span-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, ("known_human".to_string(), "replace".to_string()));
    }

    #[test]
    fn an_inserted_span_remembers_its_original_size() {
        let (_dir, db) = db();

        db.insert_span(&new_span("span-1")).unwrap();

        let total: i64 = db
            .lock()
            .query_row(
                "SELECT total_bytes FROM span WHERE span_id = 'span-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 25);
    }

    #[test]
    fn live_spans_lists_the_pieces_of_one_file_in_order() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        let mut other_file = new_span("span-2");
        other_file.file_path = "src/b.rs".to_string();
        db.insert_span(&other_file).unwrap();

        let live = db.live_spans("/repo/.git", "src/a.rs").unwrap();

        assert_eq!(live.len(), 1);
        assert_eq!(live[0].span_id, "span-1");
        assert_eq!(live[0].author_id, "author-1");
        assert_eq!(live[0].pieces, vec![(0, 10), (40, 55)]);
    }

    #[test]
    fn a_remap_replaces_the_pieces_and_appends_a_touch() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();

        db.apply_remaps(&[SpanRemap {
            span_id: "span-1".to_string(),
            pieces: vec![(5, 10)],
            touch: Some(SpanTouch {
                touched_at: 2000,
                toucher_kind: "known_human".to_string(),
                toucher_id: "human-1".to_string(),
                touch_kind: TouchKind::Replace,
                bytes_removed: 20,
            }),
        }])
        .unwrap();

        let live = db.live_spans("/repo/.git", "src/a.rs").unwrap();
        assert_eq!(live[0].pieces, vec![(5, 10)]);
        assert_eq!(row_count(&db, "span_touch"), 1);
    }

    #[test]
    fn a_span_with_no_pieces_left_is_no_longer_live() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();

        db.apply_remaps(&[SpanRemap {
            span_id: "span-1".to_string(),
            pieces: Vec::new(),
            touch: None,
        }])
        .unwrap();

        assert!(db.live_spans("/repo/.git", "src/a.rs").unwrap().is_empty());
        assert_eq!(row_count(&db, "span"), 1);
    }

    fn pin_row(db: &FactsDatabase, span_id: &str) -> (Option<String>, Option<String>, Option<i64>) {
        db.lock()
            .query_row(
                "SELECT pinned_commit, pinned_blob, pinned_at FROM span WHERE span_id = ?1",
                params![span_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    fn blobs(file_path: &str, blob_sha: &str) -> HashMap<String, String> {
        HashMap::from([(file_path.to_string(), blob_sha.to_string())])
    }

    #[test]
    fn pinning_stamps_every_unpinned_span_of_a_committed_file() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.insert_span(&new_span("span-2")).unwrap();

        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();

        let expected = (
            Some("commit-1".to_string()),
            Some("blob-1".to_string()),
            Some(3000),
        );
        assert_eq!(pin_row(&db, "span-1"), expected);
        assert_eq!(pin_row(&db, "span-2"), expected);
    }

    #[test]
    fn pinning_leaves_spans_of_other_files_unpinned() {
        let (_dir, db) = db();
        let mut other_file = new_span("span-2");
        other_file.file_path = "src/b.rs".to_string();
        db.insert_span(&new_span("span-1")).unwrap();
        db.insert_span(&other_file).unwrap();

        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();

        assert_eq!(pin_row(&db, "span-2"), (None, None, None));
    }

    #[test]
    fn a_pinned_span_is_never_pinned_again() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();

        db.pin_spans("/repo/.git", "commit-2", 4000, &blobs("src/a.rs", "blob-2"))
            .unwrap();

        assert_eq!(
            pin_row(&db, "span-1"),
            (
                Some("commit-1".to_string()),
                Some("blob-1".to_string()),
                Some(3000)
            )
        );
    }

    #[test]
    fn unpinned_files_lists_each_file_with_an_unpinned_span_once() {
        let (_dir, db) = db();
        let mut other_file = new_span("span-3");
        other_file.file_path = "src/b.rs".to_string();
        db.insert_span(&new_span("span-1")).unwrap();
        db.insert_span(&new_span("span-2")).unwrap();
        db.insert_span(&other_file).unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/b.rs", "blob-1"))
            .unwrap();

        let files = db.unpinned_files("/repo/.git").unwrap();

        assert_eq!(files, HashSet::from(["src/a.rs".to_string()]));
    }

    fn call_with_tool_use(request_id: &str, tool_use_id: &str) -> TranscriptFact {
        call_of(CallFact {
            request_id: request_id.to_string(),
            tool_use_ids: vec![tool_use_id.to_string()],
            ..Default::default()
        })
    }

    fn link_of(db: &FactsDatabase, span_id: &str) -> (Option<String>, String, Option<i64>) {
        db.lock()
            .query_row(
                "SELECT request_id, link_status, next_link_at FROM span WHERE span_id = ?1",
                params![span_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap()
    }

    #[test]
    fn a_span_saved_after_its_call_is_linked_at_insert() {
        let (_dir, db) = db();
        db.commit_facts(&[call_with_tool_use("req-1", "toolu_1")])
            .unwrap();

        db.insert_span(&new_span("span-1")).unwrap();

        assert_eq!(
            link_of(&db, "span-1"),
            (Some("req-1".to_string()), "linked".to_string(), None)
        );
    }

    #[test]
    fn a_span_saved_before_its_call_waits_with_a_deadline() {
        let (_dir, db) = db();

        db.insert_span(&new_span("span-1")).unwrap();

        assert_eq!(
            link_of(&db, "span-1"),
            (None, "pending".to_string(), Some(1000 + LINK_DEADLINE_SECS))
        );
    }

    #[test]
    fn a_call_saved_after_its_span_links_it() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();

        db.commit_facts(&[call_with_tool_use("req-1", "toolu_1")])
            .unwrap();

        assert_eq!(
            link_of(&db, "span-1"),
            (Some("req-1".to_string()), "linked".to_string(), None)
        );
    }

    #[test]
    fn a_call_for_another_tool_use_leaves_a_waiting_span_waiting() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();

        db.commit_facts(&[call_with_tool_use("req-2", "toolu_other")])
            .unwrap();

        assert_eq!(
            link_of(&db, "span-1"),
            (None, "pending".to_string(), Some(1000 + LINK_DEADLINE_SECS))
        );
    }

    #[test]
    fn a_span_without_a_tool_use_id_has_nothing_to_link() {
        let (_dir, db) = db();
        let mut span = new_span("span-1");
        span.tool_use_id = None;

        db.insert_span(&span).unwrap();

        assert_eq!(link_of(&db, "span-1"), (None, "none".to_string(), None));
    }

    #[test]
    fn expiring_marks_only_overdue_waiting_spans_unknown() {
        let (_dir, db) = db();
        let mut late = new_span("span-late");
        late.created_at = 5000;
        db.insert_span(&new_span("span-overdue")).unwrap();
        db.insert_span(&late).unwrap();
        db.commit_facts(&[call_with_tool_use("req-1", "toolu_linked")])
            .unwrap();
        let mut linked = new_span("span-linked");
        linked.tool_use_id = Some("toolu_linked".to_string());
        db.insert_span(&linked).unwrap();

        let expired = db.expire_unlinked_spans(3000).unwrap();

        assert_eq!(expired, 1);
        assert_eq!(
            link_of(&db, "span-overdue"),
            (None, "unknown".to_string(), None)
        );
        assert_eq!(link_of(&db, "span-late").1, "pending");
        assert_eq!(link_of(&db, "span-linked").1, "linked");
    }

    #[test]
    fn a_pinned_span_nobody_touched_survives_in_full() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();

        let survival = db.pinned_span_survival().unwrap();

        assert_eq!(survival.len(), 1);
        assert_eq!(survival[0].span_id, "span-1");
        assert_eq!(survival[0].committed_at, 3000);
        assert_eq!(survival[0].surviving_bytes, 25);
        assert_eq!(survival[0].total_bytes, 25);
    }

    #[test]
    fn a_remapped_span_survives_only_in_its_remaining_pieces() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();
        db.apply_remaps(&[SpanRemap {
            span_id: "span-1".to_string(),
            pieces: vec![(24, 30)],
            touch: None,
        }])
        .unwrap();

        let survival = db.pinned_span_survival().unwrap();

        assert_eq!(survival[0].surviving_bytes, 6);
        assert_eq!(survival[0].total_bytes, 25);
    }

    #[test]
    fn a_pinned_span_with_no_pieces_left_survives_as_zero() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();
        db.apply_remaps(&[SpanRemap {
            span_id: "span-1".to_string(),
            pieces: Vec::new(),
            touch: None,
        }])
        .unwrap();

        let survival = db.pinned_span_survival().unwrap();

        assert_eq!(survival.len(), 1);
        assert_eq!(survival[0].surviving_bytes, 0);
        assert_eq!(survival[0].total_bytes, 25);
    }

    #[test]
    fn spans_that_are_not_pinned_are_not_measured() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();

        assert!(db.pinned_span_survival().unwrap().is_empty());
    }

    #[test]
    fn a_pinned_span_with_an_unknown_size_is_not_measured() {
        let (_dir, db) = db();
        db.insert_span(&new_span("span-1")).unwrap();
        db.lock()
            .execute("UPDATE span SET total_bytes = NULL", [])
            .unwrap();
        db.pin_spans("/repo/.git", "commit-1", 3000, &blobs("src/a.rs", "blob-1"))
            .unwrap();

        assert!(db.pinned_span_survival().unwrap().is_empty());
    }

    #[test]
    fn unknown_prior_kinds_and_edit_types_are_rejected() {
        let (_dir, db) = db();
        insert_span(&db, "span-1").unwrap();

        let set = |column: &str, value: &str| {
            db.lock().execute(
                &format!("UPDATE span SET {column} = ?1 WHERE span_id = 'span-1'"),
                params![value],
            )
        };

        assert!(set("prior_kind", "robot").is_err());
        assert!(set("edit_type", "move").is_err());
        for kind in ["ai", "known_human", "untracked", "mixed", "none"] {
            set("prior_kind", kind).unwrap();
        }
        for edit_type in ["add", "replace"] {
            set("edit_type", edit_type).unwrap();
        }
    }

    fn insert_touch(
        db: &FactsDatabase,
        span_id: &str,
        touch_kind: &str,
    ) -> rusqlite::Result<usize> {
        db.lock().execute(
            "INSERT INTO span_touch (
                span_id, touched_at, toucher_kind, toucher_id, touch_kind, bytes_removed
             ) VALUES (?1, 2000, 'known_human', 'human-1', ?2, 40)",
            params![span_id, touch_kind],
        )
    }

    #[test]
    fn a_span_keeps_every_touch_made_to_it() {
        let (_dir, db) = db();

        insert_touch(&db, "span-1", "replace").unwrap();
        insert_touch(&db, "span-1", "replace").unwrap();
        insert_touch(&db, "span-1", "delete").unwrap();

        assert_eq!(row_count(&db, "span_touch"), 3);
    }

    #[test]
    fn a_touch_is_either_a_replace_or_a_delete() {
        let (_dir, db) = db();

        assert!(insert_touch(&db, "span-1", "add").is_err());
        assert_eq!(row_count(&db, "span_touch"), 0);
    }

    #[test]
    fn a_new_span_starts_pending_with_no_attempts_and_nothing_pinned() {
        let (_dir, db) = db();
        insert_span(&db, "span-1").unwrap();

        let stored: (String, i64, Option<i64>, Option<String>) = db
            .lock()
            .query_row(
                "SELECT link_status, link_attempts, next_link_at, pinned_commit
                 FROM span WHERE span_id = 'span-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(stored, ("pending".to_string(), 0, None, None));
    }

    #[test]
    fn span_ids_are_unique() {
        let (_dir, db) = db();
        insert_span(&db, "span-1").unwrap();

        assert!(insert_span(&db, "span-1").is_err());
        assert_eq!(row_count(&db, "span"), 1);
    }

    #[test]
    fn a_span_has_one_row_per_piece_index() {
        let (_dir, db) = db();
        let insert_piece = |piece_index: i64| {
            db.lock().execute(
                "INSERT INTO span_piece (span_id, piece_index, start_byte, end_byte)
                 VALUES ('span-1', ?1, 0, 10)",
                params![piece_index],
            )
        };

        insert_piece(0).unwrap();
        insert_piece(1).unwrap();

        assert!(insert_piece(1).is_err());
        assert_eq!(row_count(&db, "span_piece"), 2);
    }

    #[test]
    fn measurements_append_per_span_and_never_overwrite() {
        let (_dir, db) = db();
        let measure = |measured_at: i64| {
            db.lock().execute(
                "INSERT INTO survival_measurement
                    (span_id, measured_at, committed_at, surviving_bytes, total_bytes, outcome)
                 VALUES ('span-1', ?1, 500, 80, 100, 'ok')",
                params![measured_at],
            )
        };

        measure(1000).unwrap();
        measure(2800).unwrap();

        assert!(measure(2800).is_err());
        assert_eq!(row_count(&db, "survival_measurement"), 2);
    }

    #[test]
    fn newer_schema_versions_fail_closed_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("transcript-facts-db");
        drop(FactsDatabase::open(&path).unwrap());
        crate::sqlite::open_with_memory_limits(&path)
            .unwrap()
            .execute("INSERT INTO schema_version (version) VALUES (99)", [])
            .unwrap();

        let Err(err) = FactsDatabase::open(&path) else {
            panic!("opening a newer-schema database must fail closed");
        };
        assert!(
            err.to_string().contains("newer than this binary"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn commit_facts_stores_every_call_column() {
        let (_dir, db) = db();
        let fact = CallFact {
            request_id: "req-1".to_string(),
            message_id: Some("msg-1".to_string()),
            uuid: Some("a-1".to_string()),
            parent_uuid: Some("u-1".to_string()),
            prompt_id: Some("prompt-1".to_string()),
            model: Some("main-model".to_string()),
            advisor_model: Some("advisor-model".to_string()),
            effort: Some("medium".to_string()),
            turn_effort: Some("high".to_string()),
            speed: Some("standard".to_string()),
            stop_reason: Some("tool_use".to_string()),
            input_tokens: Some(2),
            output_tokens: Some(286),
            cache_read_tokens: Some(36109),
            cache_write_tokens: Some(500),
            cache_write_5m_tokens: Some(400),
            cache_write_1h_tokens: Some(100),
            thinking_tokens: Some(120),
            iterations_json: Some(r#"[{"type":"message"}]"#.to_string()),
            timestamp: Some(1_767_225_610),
            thinking_ms: Some(4200),
            ..Default::default()
        };

        db.commit_facts(&[call_of(fact.clone())]).unwrap();

        assert_eq!(read_call(&db, "req-1"), fact);
    }

    #[test]
    fn lines_of_one_call_merge_into_one_row_in_either_order() {
        let thinking = CallFact {
            output_tokens: Some(286),
            thinking_ms: Some(900),
            ..call_fact("req-1")
        };
        let tool_use = CallFact {
            output_tokens: Some(286),
            model: Some("main-model".to_string()),
            tool_use_ids: vec!["toolu_1".to_string()],
            ..call_fact("req-1")
        };
        let merged = CallFact {
            output_tokens: Some(286),
            thinking_ms: Some(900),
            model: Some("main-model".to_string()),
            ..call_fact("req-1")
        };

        let (_dir_a, forward) = db();
        forward
            .commit_facts(&[call_of(thinking.clone()), call_of(tool_use.clone())])
            .unwrap();
        let (_dir_b, backward) = db();
        backward
            .commit_facts(&[call_of(tool_use), call_of(thinking)])
            .unwrap();

        assert_eq!(read_call(&forward, "req-1"), merged);
        assert_eq!(read_call(&backward, "req-1"), merged);
        assert_eq!(row_count(&forward, "call"), 1);
        assert_eq!(row_count(&backward, "call"), 1);
    }

    #[test]
    fn a_later_value_never_overwrites_an_earlier_one() {
        let (_dir, db) = db();
        let first = CallFact {
            model: Some("first-model".to_string()),
            output_tokens: Some(10),
            ..call_fact("req-1")
        };
        let later = CallFact {
            model: Some("later-model".to_string()),
            output_tokens: Some(99),
            ..call_fact("req-1")
        };

        db.commit_facts(&[call_of(first), call_of(later)]).unwrap();

        let stored = read_call(&db, "req-1");
        assert_eq!(stored.model.as_deref(), Some("first-model"));
        assert_eq!(stored.output_tokens, Some(10));
    }

    #[test]
    fn committing_the_same_facts_twice_adds_no_rows() {
        let (_dir, db) = db();
        let facts = vec![
            call_of(CallFact {
                tool_use_ids: vec!["toolu_1".to_string(), "toolu_2".to_string()],
                advisor_uses: vec![AdvisorFact {
                    index: 0,
                    model: "adv".to_string(),
                    input_tokens: 1,
                    output_tokens: 2,
                    cache_read_tokens: 3,
                    cache_write_tokens: 4,
                }],
                ..call_fact("req-1")
            }),
            TranscriptFact::Turn(TurnFact {
                prompt_id: "prompt-1".to_string(),
                duration_ms: Some(1000),
                message_count: Some(3),
                timestamp: Some(1_767_225_660),
            }),
        ];

        db.commit_facts(&facts).unwrap();
        db.commit_facts(&facts).unwrap();

        assert_eq!(row_count(&db, "call"), 1);
        assert_eq!(row_count(&db, "call_tool_use"), 2);
        assert_eq!(row_count(&db, "advisor_use"), 1);
        assert_eq!(row_count(&db, "turn"), 1);
    }

    #[test]
    fn tool_use_ids_map_back_to_their_request() {
        let (_dir, db) = db();
        let fact = CallFact {
            tool_use_ids: vec!["toolu_1".to_string(), "toolu_2".to_string()],
            ..call_fact("req-1")
        };
        let other = CallFact {
            tool_use_ids: vec!["toolu_3".to_string()],
            ..call_fact("req-2")
        };

        db.commit_facts(&[call_of(fact), call_of(other)]).unwrap();

        let request_for = |tool_use_id: &str| -> String {
            db.lock()
                .query_row(
                    "SELECT request_id FROM call_tool_use WHERE tool_use_id = ?1",
                    params![tool_use_id],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(request_for("toolu_1"), "req-1");
        assert_eq!(request_for("toolu_2"), "req-1");
        assert_eq!(request_for("toolu_3"), "req-2");
    }

    #[test]
    fn advisor_uses_are_keyed_by_request_and_index() {
        let (_dir, db) = db();
        let advisor = |index: u32, model: &str, input: u64| AdvisorFact {
            index,
            model: model.to_string(),
            input_tokens: input,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        let first = CallFact {
            advisor_uses: vec![advisor(0, "adv-a", 10), advisor(1, "adv-b", 20)],
            ..call_fact("req-1")
        };
        let second = CallFact {
            advisor_uses: vec![advisor(0, "adv-c", 30)],
            ..call_fact("req-2")
        };

        db.commit_facts(&[call_of(first), call_of(second)]).unwrap();

        assert_eq!(row_count(&db, "advisor_use"), 3);
        let input_tokens = |request_id: &str, index: u32| -> i64 {
            db.lock()
                .query_row(
                    "SELECT input_tokens FROM advisor_use
                     WHERE request_id = ?1 AND advisor_index = ?2",
                    params![request_id, index],
                    |row| row.get(0),
                )
                .unwrap()
        };
        assert_eq!(input_tokens("req-1", 0), 10);
        assert_eq!(input_tokens("req-1", 1), 20);
        assert_eq!(input_tokens("req-2", 0), 30);
    }

    #[test]
    fn turn_facts_merge_by_prompt() {
        let (_dir, db) = db();
        let turn = |prompt_id: &str, duration_ms: Option<u64>, message_count: Option<u64>| {
            TranscriptFact::Turn(TurnFact {
                prompt_id: prompt_id.to_string(),
                duration_ms,
                message_count,
                timestamp: None,
            })
        };

        db.commit_facts(&[
            turn("prompt-1", Some(61000), None),
            turn("prompt-1", None, Some(14)),
            turn("prompt-2", Some(5), Some(1)),
        ])
        .unwrap();

        assert_eq!(row_count(&db, "turn"), 2);
        let stored: (i64, i64) = db
            .lock()
            .query_row(
                "SELECT duration_ms, message_count FROM turn WHERE prompt_id = 'prompt-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored, (61000, 14));
    }

    #[test]
    fn a_failed_batch_leaves_no_partial_rows() {
        let (_dir, db) = db();
        db.lock().execute_batch("DROP TABLE turn").unwrap();
        let facts = [
            call_of(call_fact("req-1")),
            TranscriptFact::Turn(TurnFact {
                prompt_id: "prompt-1".to_string(),
                duration_ms: Some(1),
                message_count: Some(1),
                timestamp: None,
            }),
        ];

        assert!(db.commit_facts(&facts).is_err());

        assert_eq!(row_count(&db, "call"), 0);
    }
}
