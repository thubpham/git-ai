use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, Transaction, named_params, params};

use super::claude_facts::{CallFact, TranscriptFact, TurnFact};
use super::db::{run_migrations, to_db_i64};
use crate::error::GitAiError;

const MIGRATIONS: &[&str] = &[r#"
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
    "#];

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

    pub fn commit_facts(&self, facts: &[TranscriptFact]) -> Result<(), GitAiError> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        for fact in facts {
            match fact {
                TranscriptFact::Call(call) => upsert_call(&tx, call)?,
                TranscriptFact::Turn(turn) => upsert_turn(&tx, turn)?,
            }
        }
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

    fn db() -> (tempfile::TempDir, FactsDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let db = FactsDatabase::open(dir.path().join("transcript-facts-db")).unwrap();
        (dir, db)
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
        assert_eq!(schema_version(&db), 1);
        drop(db);
        let db = FactsDatabase::open(&path).unwrap();
        assert_eq!(schema_version(&db), 1);
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

        db.commit_facts(&[TranscriptFact::Call(fact.clone())])
            .unwrap();

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
            .commit_facts(&[
                TranscriptFact::Call(thinking.clone()),
                TranscriptFact::Call(tool_use.clone()),
            ])
            .unwrap();
        let (_dir_b, backward) = db();
        backward
            .commit_facts(&[
                TranscriptFact::Call(tool_use),
                TranscriptFact::Call(thinking),
            ])
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

        db.commit_facts(&[TranscriptFact::Call(first), TranscriptFact::Call(later)])
            .unwrap();

        let stored = read_call(&db, "req-1");
        assert_eq!(stored.model.as_deref(), Some("first-model"));
        assert_eq!(stored.output_tokens, Some(10));
    }

    #[test]
    fn committing_the_same_facts_twice_adds_no_rows() {
        let (_dir, db) = db();
        let facts = vec![
            TranscriptFact::Call(CallFact {
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

        db.commit_facts(&[TranscriptFact::Call(fact), TranscriptFact::Call(other)])
            .unwrap();

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

        db.commit_facts(&[TranscriptFact::Call(first), TranscriptFact::Call(second)])
            .unwrap();

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
            TranscriptFact::Call(call_fact("req-1")),
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
