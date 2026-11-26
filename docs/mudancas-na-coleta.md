# Mudanças na coleta

Toda mudança que altera o **significado ou a cobertura** de uma coluna do banco
entra aqui, com data e hora. O banco é um histórico contínuo, nunca migrado
para trás: quem for treinar precisa saber a partir de quando cada coluna é
confiável, senão vai ler uma mudança de instrumentação como se fosse uma
mudança de comportamento da máquina.

Os bancos de antes de cada corte continuam válidos — só é preciso filtrar por
`ts` ao montar o conjunto de treino.

---

## 2026-11-25 23:30 (-03) — `run_delay_ms` passa a cobrir todo processo gravado

`epoch >= 1795660213`

**O que mudou.** O `/proc/PID/schedstat` agora é lido em toda amostra para
todos os processos, inclusive os que não são gravados. Antes ele só era lido
para o processo que passava no filtro `active` (`delta_ticks > 0 || RSS >= 32
MB`).

**Por quê.** O `run_delay` do `schedstat` é um acumulado desde o início do
processo, então o valor gravado é um delta entre dois ciclos. Sem ler os
processos ociosos, um processo que alternava entre ativo e ocioso voltava sem
base e gravava `NULL`. Isso atingia **13,7%** das linhas de `processes`, e
justamente as mais interessantes: processos intermitentes e de rajada curta.

**Efeito nos dados.** A partir do corte, `run_delay_ms` é `NULL` apenas na
primeira amostra em que um PID aparece (ou se o processo morreu no meio da
varredura). Medido nos primeiros 13 min depois da mudança: **43 nulos em 7.262
linhas (0,59%), e os 43 são a primeira aparição de um PID** — nenhum nulo por
falta de leitura anterior. Era 13,7%.

**Custo.** O `schedstat` passou a ser lido para os ~350 processos da máquina em
vez de só para os ~47 que entram em cada amostra. A CPU do coletor foi de
~0,15% para **0,25%** de um núcleo, com RSS de 7,5 MB. Continua folgado dentro
do teto de 1% e 30 MB.

**Ao treinar.** Não comparar taxa de nulos, nem contagem de amostras com
`run_delay_ms` preenchido, entre antes e depois deste corte. Os valores em si
são a mesma grandeza e continuam comparáveis.

**Não mudou:** o esquema do banco, as linhas já gravadas, o filtro de quais
processos entram na amostra (`active` segue valendo para isso), nem `swap_kb` e
`cgroup`, que não são delta e continuam lidos só para os processos gravados.

### Na mesma mudança (sem efeito sobre colunas já coletadas)

- **Foco.** O socket do compositor passa a ser procurado sempre varrendo o
  `XDG_RUNTIME_DIR`, com `HYPRLAND_INSTANCE_SIGNATURE` e `NIRI_SOCKET` só como
  último recurso. Antes as variáveis vinham primeiro, o que não servia ao
  serviço do systemd: ele pode subir antes do compositor e não herda o ambiente
  da sessão gráfica. Agora o serviço acha o compositor que subir depois, e
  sobrevive à troca de compositor sem reiniciar. `focus_app` nulo continua
  significando "nenhuma janela focada", não "não achei o compositor": na
  medição pós-mudança os nulos vieram em rajadas de 1 a 2 amostras, sempre em
  cima de uma troca de janela ou do lançamento de um app, que é quando o
  compositor de fato fica sem janela focada. A taxa de nulos de `focus_app`
  acompanha o quanto se troca de app na janela observada (0,8% em 12 h de
  sessão parada, 18% em 13 min abrindo jogos), então ela não serve como
  indicador de saúde do IPC.
- **Teto de memória.** O limite de 30 MB do `CLAUDE.md` é de RSS, não do
  `memory.peak` do cgroup, que inclui o page cache do SQLite.

---

## 2026-11-25 07:02 (-03) — colunas novas

`epoch >= 1795600921`

Primeira amostra com `pgmajfault`, `pswpin`, `pswpout`, `pgscan_direct`,
`pgsteal_direct`, `focus_app` e `focus_pid` em `samples`, e `start_time`,
`run_delay_ms`, `majflt`, `swap_kb` e `cgroup_id` em `processes`. Tudo que é
anterior a este corte tem essas colunas nulas — são as 9.573 primeiras amostras
e 350.750 linhas de `processes`, coletadas desde 2026-11-24 07:13.

O banco de antes da migração está em
`~/.local/share/neurosched/collector-pre-migracao.db`.

---

## Buracos conhecidos na série

- **2026-11-25, 18:59 → 22:57 (~4 h).** Sem amostras. Não é falha do coletor: a
  máquina rebootou às 18:59:33 e ficou parada no pedido da senha do LUKS no
  initrd até 22:53:18, quando a senha foi digitada (a primeira tentativa falhou
  às 22:53:20; a segunda, às 22:53:27, abriu). O serviço subiu às 22:57:36, com
  o login. O `ns-collector.service` já é `WantedBy=default.target` e sobe junto
  com a sessão do usuário — não há o que corrigir no serviço.
