# allocator/memory

O lado de RAM do agente: `memory.low` para proteger o app em foco, `memory.high`
e `memory.reclaim` para segurar e liberar os apps de fundo.

Decide a partir das previsões do modelo — qual app vem a seguir e quanto de RAM
ele vai pedir, e se há aperto de RAM chegando nos próximos minutos.

Nunca zera a RAM de um processo, e os limites são restaurados ao sair.
