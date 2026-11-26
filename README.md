# Neuro-Scheduler

IA que aprende o uso diário do PC e ajusta a alocação de CPU e RAM no Linux.
O kernel continua sendo o scheduler: ele decide primeiro, e o NeuroSched só
realoca recursos depois.

- `collector/`: Rust. Coleta telemetria para o treino e grava em SQLite.
- `model/`: Python. Análise dos dados, treino e avaliação dos modelos.
- `allocator/`: Rust. O agente: recebe eventos, consulta o modelo e aplica as
  realocações de CPU e RAM.
- `docs/`: arquitetura e decisões.

Fases: 1. observa · 2. sugere · 3. age. **Fase atual: 1** — o collector acumula
de 1 a 2 semanas de dados e nada é alterado no sistema.

GPL-2.0.
