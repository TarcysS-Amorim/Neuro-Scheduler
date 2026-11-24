# collector

`ns-collector` — fase 1 do NeuroSched: **só observa**.

A cada 5 segundos lê `/proc` e grava uma amostra do sistema em SQLite. Não
altera nada no sistema, não roda como root e não fala com a rede. É a fonte de
dados para o treino em `model/`.

## O que é coletado

Por amostra (tabela `samples`):

- carga do sistema (`load1`, `load5`, `load15`);
- uso agregado de CPU no intervalo, em % do total de núcleos;
- RAM e swap (`mem_total_kb`, `mem_available_kb`, `swap_total_kb`, `swap_free_kb`);
- PSI de CPU e de memória, linhas `some` e `full`, com `avg10`, `avg60`,
  `avg300` e `total` (`/proc/pressure/{cpu,memory}`).

Por processo ativo (tabela `processes`): `pid`, `name`, `cpu_pct`, `rss_kb`.

## Como rodar

Tudo dentro do `nix-shell` (NixOS sem flakes):

```sh
nix-shell          # na raiz do repositório
cd collector
cargo run --release
```

Ctrl+C para parar. O binário também pode ser usado direto:

```sh
cargo build --release
./target/release/ns-collector
```

### Onde o banco é gravado

Em ordem de precedência:

1. `$NS_COLLECTOR_DB` (caminho completo do arquivo);
2. `$XDG_DATA_HOME/neurosched/collector.db`;
3. `~/.local/share/neurosched/collector.db` (padrão).

A pasta é criada se não existir. Para testar sem sujar o banco de verdade:

```sh
NS_COLLECTOR_DB=/tmp/teste.db cargo run --release
```

### Olhando os dados

```sh
sqlite3 ~/.local/share/neurosched/collector.db \
  "SELECT ts, round(cpu_pct,1), load1, mem_available_kb FROM samples
   ORDER BY ts DESC LIMIT 10;"

sqlite3 ~/.local/share/neurosched/collector.db \
  "SELECT name, round(avg(cpu_pct),2) cpu, round(avg(rss_kb)/1024.0,1) rss_mb
   FROM processes GROUP BY name ORDER BY cpu DESC LIMIT 15;"
```

### Rodar em segundo plano

Via systemd user (sem root), criando
`~/.config/systemd/user/ns-collector.service`:

```ini
[Unit]
Description=NeuroSched collector

[Service]
ExecStart=%h/Neuro-Scheduler/collector/target/release/ns-collector
Restart=on-failure

[Install]
WantedBy=default.target
```

```sh
systemctl --user daemon-reload
systemctl --user enable --now ns-collector
```

## Detalhes que afetam a leitura dos dados

- **A primeira amostra não é gravada.** Percentual de CPU é um delta entre duas
  leituras, então o primeiro ciclo só prepara o estado. O primeiro registro
  aparece depois de ~10 s.
- **`interval_secs`** guarda o tempo real decorrido desde a amostra anterior.
  Use ele em vez de assumir 5 s: se o coletor for pausado (suspend, por
  exemplo), o intervalo real é maior.
- **`cpu_pct` de processo é percentual de um núcleo** e pode passar de 100 em
  processos com várias threads. Já o `cpu_pct` da tabela `samples` é percentual
  do total de núcleos (0–100).
- **`name` vem do `comm` do kernel** e é truncado em 15 caracteres. Em NixOS
  costuma aparecer o nome do wrapper (`.kitty-wrapped`).
- **Nem todo processo entra na amostra.** Só entram os que usaram CPU no
  intervalo ou que ocupam pelo menos 32 MiB de RSS (`MIN_RSS_BYTES` em
  `src/sampler.rs`). Sem esse filtro cada ciclo gravaria as centenas de
  processos ociosos da máquina.
- **Reuso de PID é tratado**: o `starttime` do processo é comparado entre
  amostras, e um PID reciclado recomeça do zero em vez de gerar um pico falso.
- **PSI pode não existir** em kernels sem `CONFIG_PSI`; nesse caso as colunas
  ficam `NULL`. A linha `full` da CPU é sempre zero no nível de sistema, por
  decisão do kernel.

## Custo

Medido em release nesta máquina, com ~77 processos por amostra:

- CPU: ~0,1% (meta do projeto: menos de 1%);
- RSS: ~5 MB (meta: menos de 30 MB);
- banco: ~4,4 KB por amostra, ou ~77 MB por dia de coleta contínua.

O banco usa WAL, então o treino em `model/` pode ler enquanto o coletor escreve.

## Privacidade

O banco tem dados de uso pessoal (quais programas você roda e quando).
`*.db` está no `.gitignore` e **não deve ser commitado**.

## Fora de escopo

Nada de modelo nem de scheduler aqui. Fase 1 só observa.
