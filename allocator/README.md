# allocator

O agente do NeuroSched. Recebe eventos, consulta o modelo e aplica as
realocações de CPU e RAM. **Não substitui o scheduler do kernel**: o kernel
(EEVDF) decide primeiro, sem esperar o agente, e o agente corrige depois.

## Mecanismos

- CPU: `cpu.weight` e `nice`; mover processos entre os cgroups "interativo" e
  "fundo"; `cpuset` só como último recurso.
- RAM (em `memory/`): `memory.low` para proteger o app em foco, `memory.high` e
  `memory.reclaim` para segurar e liberar os apps de fundo.

Tudo é cgroup v2 e POSIX puros — nada de BPF nem de sched_ext.

## Eventos

O agente reage a eventos em vez de consultar o sistema o tempo todo:

- proc connector: nascimento de processo;
- IPC do compositor: troca de app em foco;
- gatilhos de PSI.

## Limites

- Histerese: a condição precisa durar pelo menos 3 s, e cada processo tem um
  intervalo mínimo de 10 s entre mudanças (valores iniciais, ajustáveis).
- `cpu.weight` entre 25 e 400 (padrão 100).
- Nunca zerar CPU nem RAM de um processo.
- Ao sair ou cair, restaurar todos os valores padrão, com um `ExecStopPost` no
  serviço como garantia.
- Usar só as permissões estritamente necessárias.

## Estado

Fase atual do projeto: 1 (só observa). Ainda não há código aqui — o agente é
das fases 2 (sugere) e 3 (age).
