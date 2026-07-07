# Documentação técnica — drive-sync

> English version: [../README.md](../README.md)

Documentação arquitetural do projeto. Para build/run e visão de alto nível, consulte o [README](../../README.md) raiz; para orientações a agentes e contribuidores, [AGENTS.md](../../AGENTS.md).

## Índice

- [uso.md](./uso.md) — Guia passo a passo: pré-requisitos, credenciais, `config.json`, montagem, dia a dia, symlinks, arquivos ignorados, arquivos grandes (chunking), criptografia em repouso e troubleshooting
- [arquitetura.md](./arquitetura.md) — Visão geral do projeto, camadas (incluindo chunking e criptografia), estruturas de dados e limitações conhecidas
- [fluxo-de-sincronizacao.md](./fluxo-de-sincronizacao.md) — `SyncManager`, fila causal, drain (chunking, criptografia e migrações), pull watcher e reconcile de push
- [filesystem-fuse.md](./filesystem-fuse.md) — Mapeamento inode-path, operações FUSE implementadas, buffers (RAM/spool) e fluxos de leitura/escrita
