# NeuroSched

IA que aprende o uso diário do PC e ajusta o scheduler de CPU (via sched_ext) e a paginação de RAM do Linux.

## Estrutura
- `collector/`: Rust. Coleta uso de processos, CPU, RAM e PSI e grava em SQLite.
- `model/`: Python. Treino e avaliação do modelo a partir do SQLite.
- `scheduler/`: Rust + BPF (scx_rustland_core). Scheduler controlado pela IA.
- `memory/`: ajustes de paginação de RAM com base no uso aprendido.
- `docs/`: arquitetura e decisões.

## Fases
1. Observa: só coleta dados, nada é alterado no sistema.
2. Sugere: o modelo propõe ajustes e um humano aprova.
3. Age: aplica sozinho, com fallback para o scheduler padrão.

Fase atual: 1 (collector).

## Ambiente
- NixOS sem flakes. Dependências ficam no `shell.nix` e tudo roda dentro do `nix-shell`.
- Nunca sugerir apt, pacman ou instalação global.

## Regras
- O collector precisa ser leve (meta: menos de 1% de CPU e 30 MB de RAM), porque mede o próprio sistema onde roda.
- O collector só lê `/proc` e não roda como root.
- Nunca commitar os bancos coletados (`*.db`), porque têm dados de uso pessoal.
- Licença GPL-2.0, exigida pelos programas BPF do sched_ext.
- Commits no padrão Conventional Commits (`feat:`, `fix:`, `docs:`).
- Rodar `cargo fmt` e `cargo clippy` antes de cada commit.
