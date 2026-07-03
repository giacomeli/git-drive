# drive-sync

Sistema de arquivos FUSE em Rust que monta um repositório Git como filesystem virtual, com sincronização bidirecional automática. Escritas locais viram commits + push, mudanças remotas chegam via pull periódico. Funciona como um drive "estilo Dropbox" com remote em Git (incluindo repositórios GitHub).

Status: estágio inicial (v0.1.0). Operações básicas implementadas: `create`, `unlink`, `mkdir`, `rmdir`, `rename`. Diretórios aninhados suportados. Criptografia ainda não implementada — não use para dados sensíveis em repos públicos.

## Requisitos

- Rust 1.70+ (edition 2021)
- macOS com [macFUSE](https://osxfuse.github.io/) instalado: `brew install macfuse`
- Um repositório Git local (com um branch checked-out) para montar
- Opcional: `origin` configurado para sincronização com remote

## Build & Run

```bash
cargo build
cargo test                    # todos os testes
cargo run -- -r <caminho_do_repo> -m <ponto_de_montagem>
```

Exemplo:

```bash
mkdir -p /tmp/gitmount
cargo run -- -r ~/Projects/meu-repo -m /tmp/gitmount
ls /tmp/gitmount    # arquivos do HEAD aparecem aqui
touch /tmp/gitmount/hello.txt   # após debounce, commit + push automáticos
```

O diretório de montagem precisa existir antes da execução.

## Configuração

Opcional. Crie `config.json` na raiz do projeto:

```json
{
  "write_debounce_secs": 5,
  "pull_interval_secs": 60,
  "user_name": "Seu Nome",
  "user_email": "voce@example.com",
  "remote_token": null
}
```

| Campo | Default | Descrição |
| --- | --- | --- |
| `write_debounce_secs` | `5` | Segundos de ociosidade antes de aplicar escritas, commit e push. |
| `pull_interval_secs` | `60` | Intervalo de pull periódico do remote `origin`. `0` desliga. |
| `user_name` | `null` | Identidade dos commits. Se ausente, commits são pulados. |
| `user_email` | `null` | Idem. |
| `remote_token` | `null` | Token HTTPS (ex: GitHub PAT). Se ausente, usa ssh-agent / credential helper. **Texto plano — já em `.gitignore`.** |

Se o arquivo estiver ausente ou inválido, os defaults são aplicados com log de aviso.

> **Segurança:** o arquivo `config.json` já está listado no `.gitignore` por conter possivelmente um `remote_token` em texto plano. Não remova essa linha se você configurar credenciais.

## Sincronização

- **Escrita local → remote**: cada lote de operações (agrupado por debounce) vira um commit único seguido de `git push`.
- **Remote → local**: a cada `pull_interval_secs`, drive-sync faz `git fetch` + `git merge --ff-only`. Só avança se for fast-forward; em divergência, loga erro e mantém o estado local.
- **Branch**: detecta o branch ativo no boot via `git::current_branch_name` e opera nele até o unmount. HEAD detached aborta.

## Testes

```bash
cargo test                   # ~78 testes (unit + integração)
cargo test --test sync_flow  # fluxo watcher → commit → push → pull
```

Testes ponta-a-ponta que montam o kernel FUSE são **manuais** — a arquitetura usa métodos `apply_*` puros que permitem testar toda a lógica sem depender do kernel.

## Documentação

- [docs/uso.md](./docs/uso.md) — **guia passo a passo de uso** (pré-requisitos, credenciais, `config.json`, mount, dia a dia, troubleshooting)
- [CLAUDE.md](./CLAUDE.md) — guia para agentes/contribuidores
- [docs/arquitetura.md](./docs/arquitetura.md) — visão geral, camadas e estruturas de dados
- [docs/fluxo-de-sincronizacao.md](./docs/fluxo-de-sincronizacao.md) — watchers de debounce e pull
- [docs/filesystem-fuse.md](./docs/filesystem-fuse.md) — mapeamento inode hierárquico

## Licença

A definir.
