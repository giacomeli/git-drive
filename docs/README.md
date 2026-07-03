# Documentação técnica — drive-sync

Documentação arquitetural do projeto. Para build/run e visão de alto nível, consulte o [README](../README.md) raiz; para orientações ao Claude Code, [CLAUDE.md](../CLAUDE.md).

## Índice

- [uso.md](./uso.md) — Guia passo a passo: pré-requisitos, credenciais, `config.json`, montagem, operações do dia a dia e troubleshooting
- [arquitetura.md](./arquitetura.md) — Visão geral do projeto, camadas, estruturas de dados e limitações conhecidas
- [fluxo-de-sincronizacao.md](./fluxo-de-sincronizacao.md) — `SyncManager`, debounce de escrita, watcher async e buffers em memória
- [filesystem-fuse.md](./filesystem-fuse.md) — Mapeamento inode↔path, operações FUSE implementadas e fluxos de leitura/escrita
