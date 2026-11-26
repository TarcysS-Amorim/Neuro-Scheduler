# model

Análise dos dados do collector, treino e avaliação dos modelos.

## Tarefas

1. Classificar processos em interativos ou de fundo.
2. Prever o próximo app e quanto de RAM ele vai pedir.
3. Prever aperto de RAM alguns minutos antes.

## Baseline

A regra simples **"o app em foco ganha prioridade"** é obrigatória como
baseline. O modelo só vale se superar essa regra nas métricas de sucesso:
`run_delay` do app em foco, PSI (`some` e `full`, `avg10`) e major faults.

Só treinar com pelo menos 1 semana completa de dados. A comparação é sempre
contra os dados da fase 1, que são o "antes" com o scheduler padrão.
