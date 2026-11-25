# DEVLOG

Registro das decisões do NeuroSched, mais recente primeiro. O que o `git log`
já conta não se repete aqui; este arquivo guarda o **porquê** — sobretudo o das
decisões que mudam o rumo do projeto e que ninguém recupera lendo o diff.

## 2026-11-24

### Regra de arquitetura: o kernel decide, o agente corrige depois

Fixada a regra central do projeto, agora em `CLAUDE.md` na seção `Arquitetura`:

- O kernel sempre decide primeiro onde e quando cada tarefa roda. Nenhuma
  decisão de scheduling fica bloqueada esperando o agente.
- O agente observa em tempo real e corrige depois, realocando processos de
  forma duradoura: peso de CPU (`cpu.weight` e `nice`), cgroup (interativo ou
  fundo), núcleos permitidos (`cpuset`, só como último recurso) e limites de RAM.
- O agente reage a eventos — início de processo pelo proc connector, troca de
  foco pelo IPC do compositor, gatilhos de PSI — e nunca a cada decisão do
  scheduler.
- Toda realocação exige histerese: a condição precisa durar alguns segundos
  antes de valer, e cada processo tem um intervalo mínimo entre mudanças. Sem
  isso o agente oscila e atrapalha o kernel em vez de ajudar.

### sched_ext removido do projeto

Consequência direta da regra acima. O `scheduler/` era descrito como "Scheduler
sched_ext (Rust + BPF) controlado pela IA", o que é o oposto do que a regra
permite: o `scx_rustland_core` existe justamente para o userspace decidir tarefa
a tarefa, pondo o agente no caminho de decisão do kernel.

Os quatro instrumentos que o agente passa a usar — `cpu.weight`, `nice`, cgroup,
`cpuset` e limites de RAM — são cgroup v2 e POSIX puros. Nenhum deles precisa de
BPF. O `scheduler/` deixa de ser um scheduler e vira o aplicador das realocações
decididas pelo modelo.

Arquivos tocados: `CLAUDE.md` (linha de abertura, descrição do `scheduler/`,
fase 3), `scheduler/README.md` e `README.md`. Não sobrou nenhuma menção a
sched_ext, scx ou BPF no repositório.

O diretório continua chamado `scheduler/`, mesmo não sendo mais um scheduler.
Renomear fica para quando houver código lá dentro.

### Licença: segue GPL-2.0, por outro motivo

A GPL-2.0 estava registrada como "exigida pelos programas BPF do sched_ext".
Sem os programas BPF essa exigência deixou de existir, e nenhuma licença passou
a ser obrigatória — o projeto só lê `/proc` e escreve em knobs de cgroup.

Decisão: **manter GPL-2.0**, agora como escolha deliberada e não como
imposição técnica. O `LICENSE` e o `license` do `collector/Cargo.toml` ficam
intactos; só a justificativa no `CLAUDE.md` mudou.

Pesou na escolha o fato de o repositório já estar público sob GPL-2.0: trocar
para uma licença permissiva valeria apenas daí para frente, já que as versões
publicadas continuam GPL-2.0 para quem as obteve.
