//! Persistência das amostras em SQLite.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{params, Connection, Transaction};

use crate::sampler::{PressureLine, Sample};

/// Schema de um banco novo. Em banco já existente, quem acerta as colunas que
/// faltam é [`migrate`] — o `CREATE TABLE IF NOT EXISTS` não mexe em tabela que
/// já está lá.
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
    mem_full_total    INTEGER,
    -- Deltas de /proc/vmstat no intervalo, não os acumulados.
    pgmajfault        INTEGER,
    pswpin            INTEGER,
    pswpout           INTEGER,
    pgscan_direct     INTEGER,
    pgsteal_direct    INTEGER,
    -- App em foco: `class` no Hyprland, `app_id` no niri.
    focus_app         TEXT,
    focus_pid         INTEGER
);

CREATE INDEX IF NOT EXISTS samples_ts ON samples (ts);

-- Caminhos de cgroup são poucos e repetem em toda amostra, então vão em tabela
-- à parte: guardar o caminho inteiro em cada linha de `processes` quase dobraria
-- o tamanho do banco. A view `processes_full` devolve o caminho já resolvido.
CREATE TABLE IF NOT EXISTS cgroups (
    id    INTEGER PRIMARY KEY,
    path  TEXT NOT NULL UNIQUE
);

CREATE TABLE IF NOT EXISTS processes (
    sample_id     INTEGER NOT NULL REFERENCES samples (id) ON DELETE CASCADE,
    pid           INTEGER NOT NULL,
    name          TEXT    NOT NULL,
    start_time    INTEGER,
    cpu_pct       REAL    NOT NULL,
    rss_kb        INTEGER NOT NULL,
    run_delay_ms  REAL,
    majflt        INTEGER,
    swap_kb       INTEGER,
    cgroup_id     INTEGER REFERENCES cgroups (id),
    PRIMARY KEY (sample_id, pid)
) WITHOUT ROWID;

CREATE INDEX IF NOT EXISTS processes_name ON processes (name);
"#;

/// Criada depois da migração, quando `cgroup_id` já existe com certeza.
const VIEWS: &str = r#"
CREATE VIEW IF NOT EXISTS processes_full AS
SELECT p.sample_id, p.pid, p.name, p.start_time, p.cpu_pct, p.rss_kb,
       p.run_delay_ms, p.majflt, p.swap_kb, c.path AS cgroup
FROM processes p
LEFT JOIN cgroups c ON c.id = p.cgroup_id;
"#;

/// Colunas acrescentadas depois da primeira versão do coletor.
///
/// Todas entram com `ALTER TABLE ... ADD COLUMN`, que preserva as linhas já
/// gravadas: as amostras antigas ficam com NULL nas colunas novas, que é a
/// leitura correta — naquele período o dado não foi coletado.
const ADDED: &[(&str, &str, &str)] = &[
    ("samples", "pgmajfault", "INTEGER"),
    ("samples", "pswpin", "INTEGER"),
    ("samples", "pswpout", "INTEGER"),
    ("samples", "pgscan_direct", "INTEGER"),
    ("samples", "pgsteal_direct", "INTEGER"),
    ("samples", "focus_app", "TEXT"),
    ("samples", "focus_pid", "INTEGER"),
    ("processes", "start_time", "INTEGER"),
    ("processes", "run_delay_ms", "REAL"),
    ("processes", "majflt", "INTEGER"),
    ("processes", "swap_kb", "INTEGER"),
    // SQLite aceita REFERENCES em ADD COLUMN porque o default é NULL.
    ("processes", "cgroup_id", "INTEGER REFERENCES cgroups (id)"),
];

pub struct Store {
    conn: Connection,
    /// `path` -> `cgroups.id`, para não consultar a tabela a cada processo.
    cgroups: HashMap<String, i64>,
}

impl Store {
    /// Abre (ou cria) o banco, garante o schema e aplica a migração.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // WAL deixa a escrita periódica barata e não bloqueia quem estiver lendo
        // o banco em paralelo (o treino do modelo, por exemplo).
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(SCHEMA)?;
        migrate(&conn)?;
        conn.execute_batch(VIEWS)?;

        let cgroups = load_cgroups(&conn)?;
        Ok(Self { conn, cgroups })
    }

    /// Grava uma amostra e seus processos em uma única transação.
    pub fn insert(&mut self, sample: &Sample) -> rusqlite::Result<()> {
        // Campos separados para o cache e a conexão poderem ser emprestados
        // ao mesmo tempo.
        let Self { conn, cgroups } = self;

        let tx = conn.transaction()?;
        let sample_id = insert_sample(&tx, sample)?;
        // Cgroups novos só entram no cache depois do commit: se a transação
        // voltar atrás, um id em cache apontaria para linha inexistente e toda
        // amostra seguinte quebraria na chave estrangeira.
        let mut fresh = Vec::new();
        insert_processes(&tx, sample_id, sample, cgroups, &mut fresh)?;
        tx.commit()?;

        cgroups.extend(fresh);
        Ok(())
    }
}

/// Acrescenta as colunas de [`ADDED`] que ainda não existirem.
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    for (table, column, decl) in ADDED {
        if !has_column(conn, table, column)? {
            conn.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"))?;
        }
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        if row.get::<_, String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn load_cgroups(conn: &Connection) -> rusqlite::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT path, id FROM cgroups")?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    rows.collect()
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
            mem_full_avg10, mem_full_avg60, mem_full_avg300, mem_full_total,
            pgmajfault, pswpin, pswpout, pgscan_direct, pgsteal_direct,
            focus_app, focus_pid
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10,
            ?11, ?12, ?13, ?14,
            ?15, ?16, ?17, ?18,
            ?19, ?20, ?21, ?22,
            ?23, ?24, ?25, ?26,
            ?27, ?28, ?29, ?30, ?31,
            ?32, ?33
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
        s.vmstat.pgmajfault,
        s.vmstat.pswpin,
        s.vmstat.pswpout,
        s.vmstat.pgscan_direct,
        s.vmstat.pgsteal_direct,
        s.focus.app,
        s.focus.pid,
    ])?;

    Ok(tx.last_insert_rowid())
}

fn insert_processes(
    tx: &Transaction<'_>,
    sample_id: i64,
    s: &Sample,
    cache: &HashMap<String, i64>,
    fresh: &mut Vec<(String, i64)>,
) -> rusqlite::Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO processes (
            sample_id, pid, name, start_time, cpu_pct, rss_kb,
            run_delay_ms, majflt, swap_kb, cgroup_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;

    for p in &s.procs {
        let cgroup_id = match &p.cgroup {
            Some(path) => Some(cgroup_id(tx, cache, fresh, path)?),
            None => None,
        };
        stmt.execute(params![
            sample_id,
            p.pid,
            p.name,
            p.start_time,
            p.cpu_pct,
            p.rss_bytes / 1024,
            p.run_delay_ms,
            p.majflt,
            p.swap_kb,
            cgroup_id,
        ])?;
    }
    Ok(())
}

/// Id do caminho em `cgroups`, inserindo-o se for a primeira vez.
fn cgroup_id(
    tx: &Transaction<'_>,
    cache: &HashMap<String, i64>,
    fresh: &mut Vec<(String, i64)>,
    path: &str,
) -> rusqlite::Result<i64> {
    if let Some(id) = cache.get(path) {
        return Ok(*id);
    }
    if let Some((_, id)) = fresh.iter().find(|(p, _)| p == path) {
        return Ok(*id);
    }

    // O `DO UPDATE` não muda nada: existe para o `RETURNING` devolver o id
    // também quando o caminho já estava no banco mas fora do cache.
    let id = tx
        .prepare_cached(
            "INSERT INTO cgroups (path) VALUES (?1)
             ON CONFLICT (path) DO UPDATE SET path = excluded.path
             RETURNING id",
        )?
        .query_row([path], |r| r.get(0))?;

    fresh.push((path.to_string(), id));
    Ok(id)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::focus::FocusInfo;
    use crate::sampler::{ProcSample, VmstatDelta};

    /// Schema do coletor v0, copiado do commit f600f76. É a única forma de
    /// banco que existiu antes desta migração.
    const V0_SCHEMA: &str = r#"
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

    fn sample() -> Sample {
        Sample {
            ts: 1,
            interval_secs: 5.0,
            load1: 0.0,
            load5: 0.0,
            load15: 0.0,
            cpu_pct: 0.0,
            mem_total_kb: 1,
            mem_available_kb: None,
            swap_total_kb: 0,
            swap_free_kb: 0,
            cpu_pressure: Default::default(),
            mem_pressure: Default::default(),
            vmstat: VmstatDelta {
                pgmajfault: Some(7),
                ..Default::default()
            },
            focus: FocusInfo {
                app: Some("kitty".into()),
                pid: Some(42),
            },
            procs: vec![ProcSample {
                pid: 42,
                name: "kitty".into(),
                start_time: 1000,
                cpu_pct: 1.0,
                rss_bytes: 2048,
                run_delay_ms: Some(0.5),
                majflt: Some(3),
                swap_kb: Some(9),
                cgroup: Some("/user.slice/app.slice".into()),
            }],
        }
    }

    /// O banco da fase 1 é longo demais para ser recriado: a migração precisa
    /// acrescentar as colunas sem tocar nas linhas já gravadas.
    #[test]
    fn migracao_preserva_as_linhas_antigas() {
        let dir = std::env::temp_dir().join(format!("ns-collector-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("old.db");
        let _ = std::fs::remove_file(&path);

        // Banco exatamente na forma do coletor v0 (commit f600f76), que é a do
        // banco em produção: uma amostra e um processo já gravados.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(V0_SCHEMA).unwrap();
            conn.execute_batch(
                "INSERT INTO samples (
                    id, ts, interval_secs, load1, load5, load15, cpu_pct,
                    mem_total_kb, mem_available_kb, swap_total_kb, swap_free_kb
                 ) VALUES (1, 111, 5.0, 0, 0, 0, 0, 1, NULL, 0, 0);
                 INSERT INTO processes (sample_id, pid, name, cpu_pct, rss_kb)
                 VALUES (1, 7, 'antigo', 2.0, 100);",
            )
            .unwrap();
        }

        let mut store = Store::open(&path).unwrap();
        store.insert(&sample()).unwrap();

        let conn = &store.conn;
        // A linha antiga continua lá, com NULL no que não foi coletado.
        let (ts, focus): (i64, Option<String>) = conn
            .query_row("SELECT ts, focus_app FROM samples WHERE id = 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(ts, 111);
        assert_eq!(focus, None);

        let name: String = conn
            .query_row("SELECT name FROM processes WHERE sample_id = 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(name, "antigo");

        // E a amostra nova chegou completa.
        let (app, pid, pgmajfault): (String, i64, i64) = conn
            .query_row(
                "SELECT focus_app, focus_pid, pgmajfault FROM samples WHERE id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!((app.as_str(), pid, pgmajfault), ("kitty", 42, 7));

        let (cgroup, run_delay, swap): (String, f64, i64) = conn
            .query_row(
                "SELECT cgroup, run_delay_ms, swap_kb FROM processes_full WHERE sample_id = 2",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(cgroup, "/user.slice/app.slice");
        assert_eq!((run_delay, swap), (0.5, 9));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Duas amostras com o mesmo cgroup não podem criar dois ids.
    #[test]
    fn cgroup_nao_duplica() {
        let dir = std::env::temp_dir().join(format!("ns-collector-cg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("new.db");
        let _ = std::fs::remove_file(&path);

        let mut store = Store::open(&path).unwrap();
        store.insert(&sample()).unwrap();
        let mut second = sample();
        second.ts = 2;
        store.insert(&second).unwrap();

        let n: i64 = store
            .conn
            .query_row("SELECT count(*) FROM cgroups", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
