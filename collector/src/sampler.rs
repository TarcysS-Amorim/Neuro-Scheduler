//! Leitura de `/proc` via crate `procfs`.
//!
//! O coletor só lê. Nada aqui escreve no sistema nem precisa de root.

use std::collections::HashMap;
use std::time::Instant;

use procfs::{Current, CurrentSI, LoadAverage, Meminfo};

use crate::focus::{Focus, FocusInfo};

/// Um processo só entra na amostra se usou CPU no intervalo ou se ocupa pelo
/// menos este tanto de RSS. Sem esse filtro, cada ciclo gravaria as centenas de
/// processos ociosos da máquina e o banco cresceria rápido demais.
const MIN_RSS_BYTES: i64 = 32 * 1024 * 1024;

/// Contadores de `/proc/vmstat` que viram delta por amostra. A ordem é a das
/// colunas em [`VmstatDelta`]. A comparação é por chave exata, senão
/// `pgscan_direct` casaria também com `pgscan_direct_throttle`.
const VMSTAT_KEYS: [&str; 5] = [
    "pgmajfault",
    "pswpin",
    "pswpout",
    "pgscan_direct",
    "pgsteal_direct",
];

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

/// Deltas de `/proc/vmstat` no intervalo entre duas amostras.
///
/// Cada campo é `None` quando não havia leitura anterior (primeira amostra) ou
/// quando o contador andou para trás, o que só acontece se o kernel zerá-lo.
/// Guardar o delta, e não o acumulado, evita que o modelo precise diferenciar
/// as linhas por conta própria.
#[derive(Debug, Default, Clone, Copy)]
pub struct VmstatDelta {
    pub pgmajfault: Option<i64>,
    pub pswpin: Option<i64>,
    pub pswpout: Option<i64>,
    pub pgscan_direct: Option<i64>,
    pub pgsteal_direct: Option<i64>,
}

/// Uso de um processo no intervalo entre duas amostras.
#[derive(Debug)]
pub struct ProcSample {
    pub pid: i32,
    pub name: String,
    /// Início do processo em Unix epoch (segundos), derivado do `starttime` de
    /// `/proc/PID/stat` somado ao boot. Em epoch, e não em ticks desde o boot,
    /// o valor continua comparável depois de reiniciar a máquina; junto com o
    /// `pid` é o que identifica o processo entre amostras.
    pub start_time: i64,
    /// Percentual de um núcleo (pode passar de 100 em processos com threads).
    pub cpu_pct: f64,
    pub rss_bytes: i64,
    /// Tempo de espera na runqueue **no intervalo**, de `/proc/PID/schedstat`.
    ///
    /// `None` só na primeira vez que o PID é visto (sem base anterior) ou se o
    /// `schedstat` sumiu. O kernel conta aqui só a tarefa líder do grupo, não a
    /// soma das threads: em apps muito paralelos o valor é um piso.
    pub run_delay_ms: Option<f64>,
    /// Major faults **no intervalo**, do `majflt` de `/proc/PID/stat`.
    pub majflt: Option<i64>,
    /// `VmSwap` de `/proc/PID/status`: quanto do processo está no swap agora.
    pub swap_kb: Option<i64>,
    /// Caminho do cgroup v2 (`/proc/PID/cgroup`).
    pub cgroup: Option<String>,
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
    pub vmstat: VmstatDelta,
    /// App em foco no momento da amostra, pelo IPC do compositor.
    pub focus: FocusInfo,
    pub procs: Vec<ProcSample>,
}

/// Estado guardado entre amostras para calcular deltas de CPU.
struct PrevProc {
    /// Usado para detectar reuso de PID.
    starttime: u64,
    cpu_ticks: u64,
    majflt: u64,
    /// Lido para todo processo visto no ciclo, ativo ou não, porque é a base
    /// do delta do ciclo seguinte. `None` só quando o `schedstat` não pôde ser
    /// lido (processo morreu no meio da varredura).
    run_delay_ns: Option<u64>,
}

struct PrevCpu {
    total: u64,
    idle: u64,
}

pub struct Sampler {
    ticks_per_sec: f64,
    page_size: u64,
    /// Unix epoch do boot, para converter o `starttime` dos processos.
    boot_time_secs: i64,
    prev_procs: HashMap<i32, PrevProc>,
    prev_cpu: Option<PrevCpu>,
    prev_vmstat: Option<[Option<u64>; 5]>,
    prev_at: Option<Instant>,
    focus: Focus,
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            ticks_per_sec: procfs::ticks_per_second() as f64,
            page_size: procfs::page_size(),
            boot_time_secs: procfs::boot_time_secs().unwrap_or(0) as i64,
            prev_procs: HashMap::new(),
            prev_cpu: None,
            prev_vmstat: None,
            prev_at: None,
            focus: Focus::new(),
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
        let vmstat = self.vmstat_delta();
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
            vmstat,
            focus: self.focus.current(),
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

    /// Deltas dos contadores de `/proc/vmstat` desde a amostra anterior.
    fn vmstat_delta(&mut self) -> VmstatDelta {
        let cur = read_vmstat();

        let delta = match (self.prev_vmstat.as_ref(), cur.as_ref()) {
            (Some(prev), Some(cur)) => {
                // Contador que andou para trás (kernel zerou) vira NULL em vez
                // de um salto absurdo.
                let d = |i: usize| {
                    cur[i]
                        .zip(prev[i])
                        .and_then(|(c, p)| c.checked_sub(p))
                        .map(|v| v as i64)
                };
                VmstatDelta {
                    pgmajfault: d(0),
                    pswpin: d(1),
                    pswpout: d(2),
                    pgscan_direct: d(3),
                    pgsteal_direct: d(4),
                }
            }
            _ => VmstatDelta::default(),
        };

        // Leitura falha não apaga a base: o próximo delta cobre o período todo.
        if cur.is_some() {
            self.prev_vmstat = cur;
        }
        delta
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
            let majflt = prev.and_then(|p| stat.majflt.checked_sub(p.majflt).map(|v| v as i64));
            let prev_run_delay = prev.and_then(|p| p.run_delay_ns);

            let active = delta_ticks > 0 || rss_bytes >= MIN_RSS_BYTES;

            // O `schedstat` é lido para **todo** processo, inclusive os que não
            // vão ser gravados. O valor é um acumulado desde o início do
            // processo, então o delta exige a leitura do ciclo anterior: sem
            // ler os ociosos, todo processo que alternava entre ativo e ocioso
            // voltava sem base e gravava `run_delay_ms` nulo. Era 13,7% das
            // linhas, concentrado justamente nos processos intermitentes.
            //
            // `status` e `cgroup` continuam só para os gravados: nenhum dos
            // dois é delta, logo não precisam de base do ciclo anterior.
            let run_delay_ns = proc.schedstat().ok().map(|s| s.run_delay);

            current.insert(
                pid,
                PrevProc {
                    starttime: stat.starttime,
                    cpu_ticks,
                    majflt: stat.majflt,
                    run_delay_ns,
                },
            );

            // Na primeira amostra só aquecemos o estado; nada é gravado.
            let Some(elapsed) = elapsed else { continue };
            if !active {
                continue;
            }

            out.push(ProcSample {
                pid,
                name: stat.comm,
                start_time: self.boot_time_secs
                    + (stat.starttime as f64 / self.ticks_per_sec) as i64,
                cpu_pct: delta_ticks as f64 / self.ticks_per_sec / elapsed * 100.0,
                rss_bytes,
                run_delay_ms: run_delay_ns
                    .zip(prev_run_delay)
                    .and_then(|(cur, prev)| cur.checked_sub(prev))
                    .map(|ns| ns as f64 / 1_000_000.0),
                majflt,
                swap_kb: read_swap_kb(pid),
                cgroup: read_cgroup(pid),
            });
        }

        self.prev_procs = current;
        out
    }
}

/// Contadores de `/proc/vmstat` listados em [`VMSTAT_KEYS`], na mesma ordem.
///
/// O arquivo é lido à mão em vez de `procfs::vmstat()` porque aquela função
/// devolve um `HashMap` com as ~200 linhas do arquivo; aqui só cinco importam.
fn read_vmstat() -> Option<[Option<u64>; 5]> {
    let text = std::fs::read_to_string("/proc/vmstat").ok()?;
    let mut out = [None; 5];
    for line in text.lines() {
        let Some((key, value)) = line.split_once(' ') else {
            continue;
        };
        if let Some(i) = VMSTAT_KEYS.iter().position(|k| *k == key) {
            out[i] = value.trim().parse().ok();
        }
    }
    Some(out)
}

/// `VmSwap` de `/proc/<pid>/status`, em KiB.
///
/// Só essa linha interessa, então o arquivo é varrido à mão: `Process::status()`
/// montaria um `HashMap` e um struct com dezenas de `String` por processo.
fn read_swap_kb(pid: i32) -> Option<i64> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("VmSwap:"))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok())
}

/// Caminho do cgroup v2 de `/proc/<pid>/cgroup`, a linha `0::<caminho>`.
fn read_cgroup(pid: i32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
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
