//! Leitura de `/proc` via crate `procfs`.
//!
//! O coletor só lê. Nada aqui escreve no sistema nem precisa de root.

use std::collections::HashMap;
use std::time::Instant;

use procfs::{Current, CurrentSI, LoadAverage, Meminfo};

/// Um processo só entra na amostra se usou CPU no intervalo ou se ocupa pelo
/// menos este tanto de RSS. Sem esse filtro, cada ciclo gravaria as centenas de
/// processos ociosos da máquina e o banco cresceria rápido demais.
const MIN_RSS_BYTES: i64 = 32 * 1024 * 1024;

/// Registro de pressão (PSI) de uma linha de `/proc/pressure/*`.
#[derive(Debug, Clone, Copy)]
pub struct PressureLine {
    pub avg10: f32,
    pub avg60: f32,
    pub avg300: f32,
    pub total: i64,
}

impl From<&procfs::PressureRecord> for PressureLine {
    fn from(r: &procfs::PressureRecord) -> Self {
        Self {
            avg10: r.avg10,
            avg60: r.avg60,
            avg300: r.avg300,
            total: r.total as i64,
        }
    }
}

/// PSI de um recurso: a linha `some` e, quando o kernel expõe, a linha `full`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pressure {
    pub some: Option<PressureLine>,
    pub full: Option<PressureLine>,
}

/// Uso de um processo no intervalo entre duas amostras.
#[derive(Debug)]
pub struct ProcSample {
    pub pid: i32,
    pub name: String,
    /// Percentual de um núcleo (pode passar de 100 em processos com threads).
    pub cpu_pct: f64,
    pub rss_bytes: i64,
}

/// Uma amostra completa do sistema.
#[derive(Debug)]
pub struct Sample {
    /// Unix epoch em segundos.
    pub ts: i64,
    /// Segundos realmente decorridos desde a amostra anterior.
    pub interval_secs: f64,
    pub load1: f64,
    pub load5: f64,
    pub load15: f64,
    /// Uso agregado da CPU no intervalo, em percentual do total de núcleos.
    pub cpu_pct: f64,
    pub mem_total_kb: i64,
    pub mem_available_kb: Option<i64>,
    pub swap_total_kb: i64,
    pub swap_free_kb: i64,
    pub cpu_pressure: Pressure,
    pub mem_pressure: Pressure,
    pub procs: Vec<ProcSample>,
}

/// Estado guardado entre amostras para calcular deltas de CPU.
struct PrevProc {
    /// Usado para detectar reuso de PID.
    starttime: u64,
    cpu_ticks: u64,
}

struct PrevCpu {
    total: u64,
    idle: u64,
}

pub struct Sampler {
    ticks_per_sec: f64,
    page_size: u64,
    prev_procs: HashMap<i32, PrevProc>,
    prev_cpu: Option<PrevCpu>,
    prev_at: Option<Instant>,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            ticks_per_sec: procfs::ticks_per_second() as f64,
            page_size: procfs::page_size(),
            prev_procs: HashMap::new(),
            prev_cpu: None,
            prev_at: None,
        }
    }

    /// Lê `/proc` uma vez.
    ///
    /// Retorna `None` na primeira chamada: sem uma leitura anterior não há como
    /// calcular percentual de CPU, então o primeiro ciclo só prepara os deltas.
    pub fn sample(&mut self) -> Result<Option<Sample>, procfs::ProcError> {
        let now = Instant::now();
        let elapsed = self.prev_at.map(|t| (now - t).as_secs_f64());

        // As leituras que guardam estado entre amostras vêm primeiro, junto com
        // `prev_at`: se alguma falhar, o relógio não avança e o próximo intervalo
        // cobre o período inteiro, mantendo os deltas de CPU coerentes.
        let cpu_pct = self.cpu_total_pct()?;
        let procs = self.scan_processes(elapsed);
        self.prev_at = Some(now);

        let Some(interval_secs) = elapsed else {
            return Ok(None);
        };

        let load = LoadAverage::current()?;
        let meminfo = Meminfo::current()?;

        Ok(Some(Sample {
            ts: unix_now(),
            interval_secs,
            load1: load.one as f64,
            load5: load.five as f64,
            load15: load.fifteen as f64,
            cpu_pct: cpu_pct.unwrap_or(0.0),
            mem_total_kb: (meminfo.mem_total / 1024) as i64,
            mem_available_kb: meminfo.mem_available.map(|v| (v / 1024) as i64),
            swap_total_kb: (meminfo.swap_total / 1024) as i64,
            swap_free_kb: (meminfo.swap_free / 1024) as i64,
            cpu_pressure: read_cpu_pressure(),
            mem_pressure: read_mem_pressure(),
            procs,
        }))
    }

    /// Uso agregado da CPU a partir dos contadores de `/proc/stat`.
    fn cpu_total_pct(&mut self) -> Result<Option<f64>, procfs::ProcError> {
        let cpu = procfs::KernelStats::current()?.total;
        let idle = cpu.idle + cpu.iowait.unwrap_or(0);
        let total = cpu.user
            + cpu.nice
            + cpu.system
            + cpu.idle
            + cpu.iowait.unwrap_or(0)
            + cpu.irq.unwrap_or(0)
            + cpu.softirq.unwrap_or(0)
            + cpu.steal.unwrap_or(0);

        let pct = self.prev_cpu.as_ref().and_then(|prev| {
            let d_total = total.saturating_sub(prev.total);
            let d_idle = idle.saturating_sub(prev.idle);
            (d_total > 0).then(|| (1.0 - d_idle as f64 / d_total as f64) * 100.0)
        });

        self.prev_cpu = Some(PrevCpu { total, idle });
        Ok(pct)
    }

    /// Percorre `/proc/<pid>` e devolve os processos considerados ativos.
    fn scan_processes(&mut self, elapsed: Option<f64>) -> Vec<ProcSample> {
        let Ok(all) = procfs::process::all_processes() else {
            return Vec::new();
        };

        let mut current: HashMap<i32, PrevProc> = HashMap::with_capacity(self.prev_procs.len());
        let mut out = Vec::new();

        for proc in all.flatten() {
            // Processos que morrem no meio da varredura simplesmente somem.
            let Ok(stat) = proc.stat() else { continue };
            let pid = proc.pid();
            let cpu_ticks = stat.utime + stat.stime;
            let rss_bytes = (stat.rss * self.page_size) as i64;

            let prev = self
                .prev_procs
                .get(&pid)
                .filter(|p| p.starttime == stat.starttime);
            let delta_ticks = prev.map_or(0, |p| cpu_ticks.saturating_sub(p.cpu_ticks));

            current.insert(
                pid,
                PrevProc {
                    starttime: stat.starttime,
                    cpu_ticks,
                },
            );

            // Na primeira amostra só aquecemos o estado; nada é gravado.
            let Some(elapsed) = elapsed else { continue };
            if delta_ticks == 0 && rss_bytes < MIN_RSS_BYTES {
                continue;
            }

            out.push(ProcSample {
                pid,
                name: stat.comm,
                cpu_pct: delta_ticks as f64 / self.ticks_per_sec / elapsed * 100.0,
                rss_bytes,
            });
        }

        self.prev_procs = current;
        out
    }
}

fn read_cpu_pressure() -> Pressure {
    match procfs::CpuPressure::current() {
        Ok(p) => Pressure {
            // No nível de sistema o kernel sempre zera a linha `full` da CPU,
            // mas gravamos assim mesmo para o schema ficar uniforme.
            some: Some((&p.some).into()),
            full: Some((&p.full).into()),
        },
        Err(_) => Pressure::default(),
    }
}

fn read_mem_pressure() -> Pressure {
    match procfs::MemoryPressure::current() {
        Ok(p) => Pressure {
            some: Some((&p.some).into()),
            full: Some((&p.full).into()),
        },
        Err(_) => Pressure::default(),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
