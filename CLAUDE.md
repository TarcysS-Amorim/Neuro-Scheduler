# NeuroSched

IA que aprende o uso diário do PC e realoca processos do Linux — peso de CPU, cgroup, núcleos e limites de RAM —, além da paginação de RAM. Ela nunca entra no caminho de decisão do kernel.

## Estrutura
- `collector/`: Rust. Coleta uso de processos, CPU, RAM e PSI e grava em SQLite.
- `model/`: Python. Treino e avaliação do modelo a partir do SQLite.
- `scheduler/`: Rust. Aplica as realocações decididas pelo modelo. Não substitui o scheduler do kernel.
- `memory/`: ajustes de paginação de RAM com base no uso aprendido.
- `docs/`: arquitetura e decisões.

## Arquitetura

Regra central: **o kernel decide, o agente corrige depois.**

- O kernel sempre decide primeiro onde e quando cada tarefa roda. Ele nunca espera o agente, e nenhuma decisão de scheduling fica bloqueada esperando resposta.
- O agente observa em tempo real e corrige depois, realocando processos de forma duradoura. Os instrumentos são:
  - peso de CPU (`cpu.weight` e `nice`);
  - cgroup (interativo ou fundo);
  - núcleos permitidos (`cpuset`), só como último recurso;
  - limites de RAM.
- O agente reage a eventos, nunca a cada decisão do scheduler:
  - início de processo, pelo proc connector;
  - troca de foco, pelo IPC do compositor;
  - gatilhos de PSI.
- Toda realocação exige histerese: a condição precisa durar alguns segundos antes de valer, e cada processo tem um intervalo mínimo entre mudanças. Sem isso o agente oscila e atrapalha o kernel em vez de ajudar.

## Fases
1. Observa: só coleta dados, nada é alterado no sistema.
2. Sugere: o modelo propõe ajustes e um humano aprova.
3. Age: aplica sozinho, revertendo as realocações se algo sair do esperado.

Fase atual: 1 (collector).

## Ambiente
- NixOS sem flakes. Dependências ficam no `shell.nix` e tudo roda dentro do `nix-shell`.
- Nunca sugerir apt, pacman ou instalação global.

## Regras
- O collector precisa ser leve (meta: menos de 1% de CPU e 30 MB de RAM), porque mede o próprio sistema onde roda.
- O collector só lê `/proc` e não roda como root.
- Nunca commitar os bancos coletados (`*.db`), porque têm dados de uso pessoal.
- Licença GPL-2.0, por escolha do projeto.
- Commits no padrão Conventional Commits (`feat:`, `fix:`, `docs:`).
- Rodar `cargo fmt` e `cargo clippy` antes de cada commit.
