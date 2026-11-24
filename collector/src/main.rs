//! ns-collector — fase 1 do NeuroSched: só observa.
//!
//! A cada 5 s lê `/proc` (processos ativos, carga, CPU, RAM e PSI) e grava uma
//! amostra em SQLite. Não altera nada no sistema e não precisa de root.

mod db;
mod sampler;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

/// Intervalo entre amostras.
const INTERVAL: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ns-collector: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = db_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let mut store = db::Store::open(&path)?;
    let mut sampler = sampler::Sampler::new();

    eprintln!("ns-collector: gravando em {}", path.display());
    eprintln!(
        "ns-collector: intervalo de {}s, Ctrl+C para parar",
        INTERVAL.as_secs()
    );

    let mut next = Instant::now();
    loop {
        // Erros de leitura ou de escrita não derrubam o coletor: ele registra e
        // segue no próximo ciclo.
        match sampler.sample() {
            Ok(Some(sample)) => {
                if let Err(e) = store.insert(&sample) {
                    eprintln!("ns-collector: falha ao gravar amostra: {e}");
                }
            }
            // Primeira volta: só prepara os deltas de CPU.
            Ok(None) => {}
            Err(e) => eprintln!("ns-collector: falha ao ler /proc: {e}"),
        }

        // Alinha pelo horário alvo para o atraso de cada ciclo não acumular.
        next += INTERVAL;
        let now = Instant::now();
        if next <= now {
            next = now + INTERVAL;
        }
        std::thread::sleep(next - now);
    }
}

/// `$NS_COLLECTOR_DB`, senão `$XDG_DATA_HOME/neurosched/collector.db`,
/// senão `~/.local/share/neurosched/collector.db`.
fn db_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(p) = non_empty("NS_COLLECTOR_DB") {
        return Ok(PathBuf::from(p));
    }

    let base = match non_empty("XDG_DATA_HOME") {
        Some(p) => PathBuf::from(p),
        None => {
            PathBuf::from(non_empty("HOME").ok_or("HOME não está definido")?).join(".local/share")
        }
    };
    Ok(base.join("neurosched").join("collector.db"))
}

fn non_empty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}
