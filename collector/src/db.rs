//! Persistência das amostras em SQLite.

use std::path::Path;

use rusqlite::{params, Connection, Transaction};

use crate::sampler::{PressureLine, Sample};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS samples (
    id                INTEGER PRIMARY KEY,
    ts                INTEGER NOT NULL,
    interval_secs     REAL    NOT NULL,
    load1             REAL    NOT NULL,
    load5             REAL    NOT NULL,
    load15            REAL    NOT NULL,
    cpu_pct           REAL    NOT NULL,
    mem_total_kb      INTEGER NOT NULL,
    mem_available_kb  INTEGER,
    swap_total_kb     INTEGER NOT NULL,
    swap_free_kb      INTEGER NOT NULL,
    cpu_some_avg10    REAL,
    cpu_some_avg60    REAL,
    cpu_some_avg300   REAL,
    cpu_some_total    INTEGER,
    cpu_full_avg10    REAL,
    cpu_full_avg60    REAL,
    cpu_full_avg300   REAL,
    cpu_full_total    INTEGER,
    mem_some_avg10    REAL,
    mem_some_avg60    REAL,
    mem_some_avg300   REAL,
    mem_some_total    INTEGER,
    mem_full_avg10    REAL,
    mem_full_avg60    REAL,
    mem_full_avg300   REAL,
    mem_full_total    INTEGER
);

CREATE INDEX IF NOT EXISTS samples_ts ON samples (ts);

CREATE TABLE IF NOT EXISTS processes (
    sample_id  INTEGER NOT NULL REFERENCES samples (id) ON DELETE CASCADE,
    pid        INTEGER NOT NULL,
    name       TEXT    NOT NULL,
    cpu_pct    REAL    NOT NULL,
    rss_kb     INTEGER NOT NULL,
    PRIMARY KEY (sample_id, pid)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS processes_name ON processes (name);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Abre (ou cria) o banco e garante o schema.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL deixa a escrita periódica barata e não bloqueia quem estiver lendo
        // o banco em paralelo (o treino do modelo, por exemplo).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    /// Grava uma amostra e seus processos em uma única transação.
    pub fn insert(&mut self, sample: &Sample) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        let sample_id = insert_sample(&tx, sample)?;
        insert_processes(&tx, sample_id, sample)?;
        tx.commit()
    }
}

fn insert_sample(tx: &Transaction<'_>, s: &Sample) -> rusqlite::Result<i64> {
    let cs = cols(&s.cpu_pressure.some);
    let cf = cols(&s.cpu_pressure.full);
    let ms = cols(&s.mem_pressure.some);
    let mf = cols(&s.mem_pressure.full);

    tx.prepare_cached(
        "INSERT INTO samples (
            ts, interval_secs, load1, load5, load15, cpu_pct,
            mem_total_kb, mem_available_kb, swap_total_kb, swap_free_kb,
            cpu_some_avg10, cpu_some_avg60, cpu_some_avg300, cpu_some_total,
            cpu_full_avg10, cpu_full_avg60, cpu_full_avg300, cpu_full_total,
            mem_some_avg10, mem_some_avg60, mem_some_avg300, mem_some_total,
            mem_full_avg10, mem_full_avg60, mem_full_avg300, mem_full_total
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22,
            ?23, ?24, ?25, ?26
        )",
    )?
    .execute(params![
        s.ts,
        s.interval_secs,
        s.load1,
        s.load5,
        s.load15,
        s.cpu_pct,
        s.mem_total_kb,
        s.mem_available_kb,
        s.swap_total_kb,
        s.swap_free_kb,
        cs.0,
        cs.1,
        cs.2,
        cs.3,
        cf.0,
        cf.1,
        cf.2,
        cf.3,
        ms.0,
        ms.1,
        ms.2,
        ms.3,
        mf.0,
        mf.1,
        mf.2,
        mf.3,
    ])?;

    Ok(tx.last_insert_rowid())
}

fn insert_processes(tx: &Transaction<'_>, sample_id: i64, s: &Sample) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO processes (sample_id, pid, name, cpu_pct, rss_kb)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for p in &s.procs {
        stmt.execute(params![
            sample_id,
            p.pid,
            p.name,
            p.cpu_pct,
            p.rss_bytes / 1024
        ])?;
    }
    Ok(())
}

/// Colunas `(avg10, avg60, avg300, total)` de uma linha de PSI.
type PsiCols = (Option<f32>, Option<f32>, Option<f32>, Option<i64>);

/// Linha ausente (kernel sem PSI) vira NULL em todas as colunas.
fn cols(line: &Option<PressureLine>) -> PsiCols {
    match line {
        Some(l) => (Some(l.avg10), Some(l.avg60), Some(l.avg300), Some(l.total)),
        None => (None, None, None, None),
    }
}
