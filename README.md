# drive-sync

Sistema de arquivos FUSE em Rust que monta um repositório Git como filesystem virtual, com
sincronização bidirecional automática. Escritas locais viram commits + push, mudanças remotas
chegam via pull periódico. Funciona como um drive "estilo Dropbox" com remote em Git (incluindo
repositórios GitHub) — com criptografia em repouso opcional que permite usar até um **repo público
como drive privado**.

Capacidades principais:

- Sincronização bidirecional com fila causal (ordem do kernel preservada), merge 3-way com
  conflicted copies e retry automático de push rejeitado.
- Arquivos grandes quebrados em chunks content-addressed abaixo do limite de 100 MB por arquivo do
  GitHub (`.chunks/` + manifest), transparente no drive.
- Criptografia em repouso opcional (XChaCha20-Poly1305): conteúdo e nomes cifrados no repo, tudo
  claro no mount; liga/desliga no config com migração automática.
- Symlinks, mtime derivado do histórico, arquivos ignorados como local-only visíveis, Lixeira
  nativa do Finder, tray na barra de menus, notificações do macOS e LaunchAgent para montar no
  login.

## Requisitos

- Rust 1.85+ (verificado no CI)
- macOS com [macFUSE](https://osxfuse.github.io/) instalado: `brew install macfuse`
  (a suíte de testes também passa em Linux — o port do runtime está no roadmap)
- Um repositório Git local (com um branch checked-out) para montar
- Opcional: `origin` configurado para sincronização com remote

## Build & Run

```bash
cargo build
cargo test                    # suíte completa (unit + integração)
cargo run -- -r <caminho_do_repo> -m <ponto_de_montagem>
cargo run -- status           # estado do daemon
cargo run -- tray             # ícone de status na barra de menus
cargo run -- key generate     # chave da criptografia em repouso
```

Para montar automaticamente no login (LaunchAgent com KeepAlive):

```bash
cargo run -- install -r <caminho_do_repo> -m <ponto_de_montagem> --with-tray
cargo run -- uninstall        # para o daemon e remove os agentes
```

SIGTERM/eject fazem parada limpa: a fila pendente é commitada e enviada antes de sair. Conflitos de
merge, push falho e pull bloqueado geram notificação do macOS. A Lixeira do Finder funciona
nativamente (itens recuperáveis até esvaziar ou remontar).

Exemplo:

```bash
mkdir -p /tmp/gitmount
cargo run -- -r ~/Projects/meu-repo -m /tmp/gitmount
ls /tmp/gitmount    # arquivos do HEAD aparecem aqui
touch /tmp/gitmount/hello.txt   # após debounce, commit + push automáticos
```

O diretório de montagem precisa existir antes da execução.

## Configuração

Opcional. `~/.config/drive-sync/config.json` (um `./config.json` no diretório atual tem
precedência no fluxo dev):

```json
{
  "write_debounce_secs": 5,
  "pull_interval_secs": 60,
  "read_cache_mb": 64,
  "large_file_threshold_mb": 64,
  "chunk_threshold_mb": 95,
  "chunk_size_mb": 80,
  "encryption_enabled": false,
  "encryption_key_file": null,
  "user_name": "Seu Nome",
  "user_email": "voce@example.com",
  "remote_token": null
}
```

| Campo | Default | Descrição |
| --- | --- | --- |
| `write_debounce_secs` | `5` | Segundos de ociosidade antes de aplicar escritas, commit e push. |
| `pull_interval_secs` | `60` | Intervalo de pull periódico do remote `origin`. `0` desliga. |
| `read_cache_mb` | `64` | Orçamento do cache LRU de blobs de leitura (MiB). `0` desativa. |
| `large_file_threshold_mb` | `64` | Acima disso, buffers de escrita vão para spool em disco (RAM limitada). `0` desativa. |
| `chunk_threshold_mb` | `95` | Acima disso, o arquivo vira manifest + chunks em `.chunks/` (limite de 100 MB do GitHub). `0` desativa. |
| `chunk_size_mb` | `80` | Tamanho máximo de cada chunk. |
| `encryption_enabled` | `false` | Criptografia em repouso: repo opaco (conteúdo + nomes), drive claro. Mudar o valor migra automaticamente. |
| `encryption_key_file` | `null` | Arquivo da chave (default `~/.config/drive-sync/drive.key`). Gere com `drive-sync key generate`. **Perdeu a chave = perdeu os dados.** |
| `user_name` | `null` | Identidade dos commits. Se ausente, commits são pulados. |
| `user_email` | `null` | Idem. |
| `remote_token` | `null` | Token HTTPS (ex: GitHub PAT). Se ausente, usa ssh-agent / credential helper. **Texto plano.** |

Se o arquivo estiver ausente ou inválido, os defaults são aplicados com log de aviso.

> **Segurança:** `config.json` pode conter um `remote_token` em texto plano e a chave de
> criptografia mora ao lado dele — mantenha `~/.config/drive-sync/` fora de qualquer repo e com
> backup da chave em local seguro. Ao habilitar criptografia num repo já existente, a história
> Git anterior permanece legível no remote; para repo público, comece de um repo novo.

## Sincronização

- **Escrita local → remote**: cada lote de operações (agrupado por debounce) vira um commit único
  seguido de `git push`. Push rejeitado dispara reconciliação (fetch + merge 3-way + retry).
- **Remote → local**: a cada `pull_interval_secs`, `git fetch` + merge — fast-forward quando
  possível, senão merge 3-way; conflitos preservam a versão local como
  `<arquivo>.conflict-<host>-<timestamp>` e aceitam a remota como canônica.
- **Branch**: detecta o branch ativo no boot e opera nele até o unmount. HEAD detached aborta.

## Testes

```bash
cargo test                   # ~256 testes (unit + integração)
cargo test --test sync_flow  # fluxo watcher -> commit -> push -> pull
```

O CI (GitHub Actions) roda a suíte completa em macOS e Linux, clippy com a régua do projeto, os
greps de padrões proibidos e a verificação de MSRV — espelhando exatamente os comandos locais.
Testes ponta-a-ponta que montam o kernel FUSE são **manuais** — a arquitetura usa métodos
`apply_*` puros que permitem testar toda a lógica sem depender do kernel.

## Documentação

- [docs/uso.md](./docs/uso.md) — **guia passo a passo de uso** (pré-requisitos, credenciais,
  `config.json`, mount, dia a dia, criptografia, arquivos grandes, troubleshooting)
- [CLAUDE.md](./CLAUDE.md) — guia para agentes/contribuidores (invariantes, armadilhas, módulos)
- [docs/arquitetura.md](./docs/arquitetura.md) — visão geral, camadas e estruturas de dados
- [docs/fluxo-de-sincronizacao.md](./docs/fluxo-de-sincronizacao.md) — watchers de debounce e pull,
  chunking, criptografia e migrações no drain
- [docs/filesystem-fuse.md](./docs/filesystem-fuse.md) — mapeamento inode hierárquico e operações FUSE
- [specs/roadmap.md](./specs/roadmap.md) — fases executadas e direção de desenvolvimento

## Licença

A definir.
