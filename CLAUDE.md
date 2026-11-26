# NeuroSched

IA que aprende o uso diário do PC e ajusta a alocação de CPU e RAM no Linux.
O kernel continua sendo o scheduler. O NeuroSched só realoca recursos depois que o kernel já decidiu.

## Arquitetura
- O kernel (EEVDF) sempre decide primeiro onde e quando cada tarefa roda, sem esperar o agente.
- O agente fica fora do caminho crítico: observa em tempo real e corrige depois.
- Ele reage a padrões que duram (de centenas de ms a segundos), nunca a cada decisão do scheduler.
- Toda realocação é duradoura: peso de CPU, cgroup, núcleos permitidos ou limites de RAM.

## Estrutura
- `collector/`: Rust. Coleta telemetria para o treino e grava em SQLite.
- `model/`: Python. Análise dos dados, treino e avaliação dos modelos.
- `allocator/`: Rust. O agente: recebe eventos, consulta o modelo e aplica as realocações (CPU e RAM).
- `docs/`: arquitetura e decisões.

## Fases
1. Observa: só coleta dados, nada é alterado no sistema.
2. Sugere: o agente mostra o que faria, e um humano aprova.
3. Age: o agente aplica sozinho, dentro dos limites.

Fase atual: 1. O collector roda como serviço do systemd de usuário (home-manager) e acumula de 1 a 2 semanas de dados.

## Collector
- Intervalo de 5 s. Banco em `~/.local/share/neurosched/collector.db`, fora do repo.
- `samples`: carga, PSI de CPU e memória, deltas de /proc/vmstat (pgmajfault, pswpin, pswpout, pgscan_direct, pgsteal_direct) e app em foco, lido pelo socket IPC do niri ou do Hyprland.
- `processes`: pid, nome, start_time, % de CPU, RSS, run_delay_ms (de /proc/PID/schedstat), majflt, swap_kb e cgroup.
- `cpu_full_*` do PSI sempre vem zerado no nível do sistema. Isso é esperado.

## Model
- Três tarefas: (1) classificar processos em interativos ou de fundo, (2) prever o próximo app e quanto de RAM ele vai pedir, (3) prever aperto de RAM alguns minutos antes.
- Baseline obrigatória: a regra simples "o app em foco ganha prioridade". O modelo só vale se superar essa regra nas métricas abaixo.
- Só treinar com pelo menos 1 semana completa de dados.

## Allocator
- Mecanismos: `cpu.weight` e `nice`; mover processos entre cgroups "interativo" e "fundo"; `memory.low` (proteger o app em foco), `memory.high` e `memory.reclaim` (segurar e liberar os apps de fundo); `cpuset` só como último recurso.
- Eventos em vez de consultas constantes: proc connector (nascimento de processo), eventos de foco do compositor e gatilhos de PSI.
- Histerese: a condição precisa durar pelo menos 3 s, e cada processo tem um intervalo mínimo de 10 s entre mudanças (valores iniciais, ajustáveis).
- Limites: `cpu.weight` entre 25 e 400 (padrão 100). Nunca zerar CPU nem RAM de um processo.
- Ao sair ou cair, restaurar todos os valores padrão, com um ExecStopPost no serviço como garantia.
- Usar só as permissões estritamente necessárias.

## Métricas de sucesso
- `run_delay` do app em foco, PSI (some e full, avg10) e major faults.
- Sempre comparar com os dados coletados na fase 1, que são o "antes" com o scheduler padrão.

## Decisões
- Abandonamos o sched_ext e o BPF: a IA não deve substituir o scheduler, porque o tempo que ela leva para pensar atrasaria as tarefas. Corrigir depois é mais seguro: no pior caso, um peso fica errado, mas o PC não trava.

## Ambiente
- NixOS sem flakes, com home-manager. Compositores niri e Hyprland.
- Dependências ficam no `shell.nix`, e tudo roda dentro do `nix-shell`. Nunca sugerir apt, pacman ou instalação global.

## Regras
- O collector precisa ser leve (menos de 1% de CPU e 30 MB de RAM), só lê o `/proc` e não roda como root.
- O teto de 30 MB é de **RSS** do processo (`VmRSS`/`VmHWM` de `/proc/PID/status`, ou o `RSS` do `ps`). Não usar o `memory.peak`/`memory.current` do cgroup nem o "memory peak" do `systemctl status`: esses incluem o page cache do SQLite, que o kernel devolve sob pressão e não é consumo do coletor. Os dois números divergem bastante — em 25/11 o cgroup marcava 36,7 MB de pico com o RSS em 6,9 MB.
- Nunca commitar os bancos coletados (`*.db`), porque têm dados de uso pessoal.
- Licença GPL-2.0.
- Commits no padrão Conventional Commits (`feat:`, `fix:`, `docs:`).
- Rodar `cargo fmt` e `cargo clippy` antes de cada commit.
