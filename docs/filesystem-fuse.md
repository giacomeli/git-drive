# Arquitetura: Filesystem FUSE

## Visão Geral

`GitDriveFS` é a ponte entre o kernel FUSE e o conteúdo versionado do repositório Git. Mantém um modelo hierárquico com `FileEntry` e `DirEntry` ligados por referências de inode, um alocador atômico para novos inodes criados em runtime (`create`/`mkdir`) e lógica extraída em métodos `apply_*` puros que são testáveis sem montar o kernel. Leituras consultam o write buffer do inode (se existir) e caem no blob Git do HEAD; mutações atualizam índices em memória, buffers de escrita e enfileiram `SyncOp`s para o `SyncManager`.

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#E3F2FD','primaryTextColor':'#0D47A1','primaryBorderColor':'#1976D2','lineColor':'#546E7A'}}}%%
flowchart TB
    accTitle: Operacoes FUSE do GitDriveFS
    accDescr: Syscalls do kernel chegam aos metodos do trait, que delegam a funcoes apply puras; indices em memoria, write buffers e a fila do SyncManager sao os tres destinos.

    classDef kernel fill:#E3F2FD,stroke:#1976D2
    classDef pure fill:#FFF3E0,stroke:#EF6C00
    classDef state fill:#F1F8E9,stroke:#558B2F

    Kernel[(Kernel FUSE)]:::kernel

    subgraph Puras["Funções puras (testáveis sem kernel)"]
        Lookup[apply_lookup]:::pure
        Meta[apply_getattr / apply_readdir]:::pure
        Mut[apply_create / apply_unlink / apply_mkdir / apply_rmdir / apply_rename]:::pure
        WriteP[apply_write / apply_truncate / apply_flush]:::pure
    end

    Idx[índices: files_by_inode, dirs_by_inode, inodes_by_path]:::state
    Buf[write_buffers + dirty flags]:::state
    Sync[SyncManager: fila causal]:::state
    Git[git.rs: blob do HEAD]:::state

    Kernel -- "lookup / getattr / readdir" --> Lookup
    Kernel -- "create / unlink / mkdir / rmdir / rename" --> Mut
    Kernel -- "write / setattr(size) / flush" --> WriteP
    Kernel -- "read" --> Buf

    Lookup --> Idx
    Meta --> Idx
    Mut --> Idx
    Mut -- "queue_*" --> Sync
    WriteP --> Buf
    WriteP -- "queue_write (se dirty)" --> Sync
    Buf -- "fallback sem buffer" --> Git

    Lookup -. "refresh_if_needed: gc de buffers + rebuild" .-> Idx
```

---

## Mapeamento de Inodes

### Constantes (`src/fs.rs`)

```rust
const TTL: Duration = Duration::from_secs(1);
const ROOT_INODE: u64 = 1;
const FILE_MODE: u16 = 0o644;
const DIR_MODE: u16 = 0o755;
```

### Estruturas

```rust
struct FileEntry {
    inode: u64,
    path: String,     // "foo/bar/baz.txt"
    parent: u64,      // inode do diretório-pai
    size: u64,
}

struct DirEntry {
    inode: u64,
    path: String,     // "" para raiz
    parent: u64,      // 0 para raiz
    children: HashSet<u64>,
}
```

### Alocador

`next_inode: Arc<AtomicU64>` inicializa em `2 + total_inodes_na_montagem`. `alloc_inode` faz `fetch_add(1, SeqCst)`. Monotônico — inodes deletados não são reciclados. No `refresh_if_needed`, paths preservados reutilizam o inode antigo e a alocação para paths novos é lazy (`unwrap_or_else`) — refresh sem mudanças não avança o contador.

### Inicialização em `GitDriveFS::new`

1. Insere raiz (`inode 1`, path `""`).
2. Chama `git::list_dirs_at_commit` e `git::list_files_at_commit` (paths completos).
3. Diretórios são ordenados por profundidade e inseridos do raso para o profundo, garantindo que o pai exista quando o filho for inserido.
4. Arquivos usam o mesmo esquema de resolução de pai via `path.rfind('/')`.
5. Raiz é tratada especialmente (path vazio -> pai é o `inodes_by_path[""]`).

---

## Ciclo de vida do write buffer

Cada inode com escrita pendente tem um `WriteBuffer` em `write_buffers` — o conteúdo INTEIRO do arquivo, em RAM copy-on-write (`Arc` compartilhado com a fila de sync) abaixo de `large_file_threshold_mb` ou em arquivo de spool acima — e, opcionalmente, uma marca em `dirty`. Para arquivo chunkado, o buffer nasce da remontagem (decifrada, se cifrado) em streaming.

```mermaid
%%{init: {'theme':'base','themeVariables':{'primaryColor':'#E3F2FD','primaryTextColor':'#0D47A1','primaryBorderColor':'#1976D2','lineColor':'#546E7A'}}}%%
stateDiagram-v2
    accTitle: Ciclo de vida do write buffer de um inode
    accDescr: Sem buffer, o read cai no HEAD; a primeira escrita cria o buffer a partir do HEAD e marca dirty; o flush enfileira e limpa a marca; o gc descarta buffers limpos quando o HEAD passa a conter o conteudo.

    [*] --> SemBuffer: leitura direta do blob no HEAD
    SemBuffer --> Dirty: primeiro write ou truncate<br/>(buffer nasce como CÓPIA do conteúdo do HEAD)
    Dirty --> Limpo: flush enfileira a op de Write<br/>e limpa a marca
    Dirty --> Limpo: truncate / rename / exchange<br/>enfileiram o conteúdo direto
    Limpo --> Dirty: novo write
    Limpo --> SemBuffer: gc — commit_generation avançou<br/>ou refresh pós-pull
    Dirty --> SemBuffer: unlink
```

Regras que sustentam o modelo:

1. **Inicialização com conteúdo corrente** (`ensure_buffer`): o buffer nasce como cópia do blob no HEAD. Semântica POSIX — `write` em offset modifica só o intervalo escrito; `truncate` preserva o prefixo. Buffer nascendo vazio zerava tudo que não fosse reescrito (corrupção em append, páginas sujas de editor e truncate).
2. **Dirty flag por inode**: `flush` roda em todo `close(2)`, inclusive de fds abertos só para leitura. `apply_flush` só enfileira `SyncOp::Write` se o inode está dirty, e limpa a marca ao enfileirar — sem isso, um simples `cat` re-enfileirava conteúdo antigo e podia reverter uma mudança remota mergeada.
3. **GC de buffers limpos** (`refresh_if_needed`): quando `commit_generation` avança (drain commitou) ou no refresh pós-pull, buffers não-dirty são descartados — o conteúdo já está no HEAD e mantê-los esconderia mudanças remotas do `read` para sempre. Buffers dirty nunca são tocados.
4. **Operações que enfileiram direto limpam a marca**: `apply_truncate`, rename de arquivo, `exchange` e o swap implícito enfileiram o conteúdo corrente na hora, então o flush seguinte não deve duplicar a op.

---

## Operações Implementadas

| Op | Método do trait | Função pura | Comportamento |
| --- | --- | --- | --- |
| `lookup` | `lookup` | `apply_lookup(parent, name)` | Busca filho em `dirs_by_inode[parent].children` pelo `leaf_name`. Refresh antes. |
| `getattr` | `getattr` | `apply_getattr(ino)` | Retorna `FileAttr` com uid/gid do processo e mtime estável (derivado do último commit que tocou o path; sessão tem precedência). Refresh antes. |
| `setattr` | `setattr` | `apply_truncate(ino, size)` | `mode` vai para `mode_overrides` (in-memory); `size` trunca o buffer preservando conteúdo; times atualizam `times`. |
| `readdir` | `readdir` | `apply_readdir(ino)` | Lista `.`, `..` e children ordenados por nome. Refresh só na primeira página (`offset == 0`). |
| `readdirplus` | `readdirplus` | `apply_readdir(ino)` | Variante com atributos inline — Finder usa `getattrlistbulk`. |
| `open` | `open` | — | Sempre aceita, fh = ino. |
| `read` | `read` | `apply_read(ino, offset, size)` | Buffer do inode (RAM ou spool), cache de blobs, working tree (local-only) ou blob do HEAD via handle de repo persistente (miss popula o cache). Erros viram `EIO`. |
| `write` | `write` | `apply_write(ino, offset, data)` | Buffer inicializado do HEAD; escrita em offset; atualiza `entry.size`; marca dirty. |
| `flush` | `flush` | `apply_flush(ino)` | Enfileira `queue_write` só se dirty; limpa a marca; `touch`. |
| `release` | `release` | — | Apenas loga. |
| `create` | `create` | `apply_create(parent, name)` | Aloca inode, insere `FileEntry`, enfileira `queue_write(path, [])` para materializar em disco. |
| `unlink` | `unlink` | `apply_unlink(parent, name)` | Remove de índices, buffer e dirty; enfileira `queue_delete`. `EISDIR` se for diretório. |
| `mkdir` | `mkdir` | `apply_mkdir(parent, name)` | Aloca inode, insere `DirEntry`, enfileira `queue_mkdir` (paths efêmeros como `.Trashes` ficam só em memória — Lixeira nativa do Finder). |
| `rmdir` | `rmdir` | `apply_rmdir(parent, name)` | Remove inode + path, enfileira `queue_rmdir`. `ENOTEMPTY` se há children. |
| `rename` | `rename` | `apply_rename(parent, name, new_parent, new_name)` | Move inode entre parents, atualiza paths de descendentes, reindexa. Flags do `renameatx_np` (patch no vendor/fuser): `RENAME_SWAP` roteia para `apply_exchange`, `RENAME_EXCL` retorna `EEXIST` se o destino existe. Dir usa `queue_rename` — exceto endpoints efêmeros: destino efêmero expande em Delete/Rmdir recursivo (bottom-up) e origem efêmera materializa via Mkdir/Write. Arquivo vira `queue_write(dest) + queue_delete(src)`, popula buffer não-dirty e limpa dirty. Nomes vazios (fallback defensivo do parse antigo) acionam o swap implícito. |
| `exchange` | `exchange` (macOS) | `apply_exchange(parent, name, newparent, newname)` | Troca conteúdo de dois arquivos (safe save do TextEdit/iWork). Atualiza buffers, enfileira os dois writes, limpa dirty. Mesma lógica atende `rename` com `RENAME_SWAP`. |
| `link` | `link` | — | Simulado via cópia com inode novo (modelo 1 inode = 1 path). Cobre o backup transiente do safe save. |
| `readlink` | `readlink` | `apply_readlink(ino)` | Alvo do symlink (conteúdo do blob `120000`). `EINVAL` para arquivo regular. |
| `symlink` | `symlink` | `apply_symlink(parent, name, target)` | Cria entry com kind symlink, buffer com o texto do alvo e enfileira `SyncOp::Symlink` (barreira na fila causal). |
| `statfs` | `statfs` | — | Volume advertido com ~1 TiB livre — sinal para o userland, não quota. |
| `setxattr`/`getxattr`/`listxattr`/`removexattr` | idem | — | `HashMap` em memória por inode; `ENOATTR` no macOS, `ENODATA` no Linux. |

### Códigos de erro

| Erro | Quando |
| --- | --- |
| `ENOENT` | Inode/nome não existe no índice |
| `ENOTDIR` | Parent esperado como diretório é arquivo |
| `EISDIR` | Operação de arquivo aplicada a diretório (unlink, exchange) |
| `EEXIST` | `create`/`mkdir`/`link`/`rename` para destino já existente (dir) |
| `ENOTEMPTY` | `rmdir` em diretório com children |
| `EINVAL` | Nome não-UTF8 |
| `EIO` | Erro do `git2` em `read` |
| `EXDEV` | Swap implícito sem par no destino (defesa contra encodings ambíguos do exchangedata) |

Não implementadas (default `ENOSYS`): `mknod`, `fsync`, `fallocate`, `copy_file_range`.

---

## Refresh de Índice

`src/fs.rs::refresh_if_needed`, chamado no início de `lookup`, `getattr`, `readdir`/`readdirplus` (primeira página):

1. Se há ops pendentes na fila do sync (`has_pending_ops`), retorna — refresh reconstruiria os índices a partir do HEAD, descartando paths criados localmente que ainda não chegaram ao commit.
2. Se `commit_generation` avançou, descarta write buffers não-dirty (ver ciclo de vida acima).
3. Lê-e-limpa `refresh_flag`; se era `false`, retorna.
4. Descarta buffers não-dirty, reabre o repo, lista dirs + files do HEAD.
5. Preserva inodes de paths que continuam existindo; para paths novos, aloca via `next_inode` (lazy). Paths que sumiram perdem o inode.

`TTL = 1s` no kernel limita a frequência efetiva dessas chamadas.

---

## Leitura de Arquivos

`apply_read` em `src/fs.rs` (callback `read` delega). Ordem das fontes:

1. **Write buffer** do inode (mutações pendentes, conteúdo pós-create/rename ainda não commitado).
2. **Cache de blobs** (`BlobCache`): LRU keyed por inode com orçamento em bytes (`read_cache_mb` no
   `config.json`, default 64 MiB; `0` desativa). Blob maior que o orçamento não entra.
3. **Blob do HEAD**: reabre o repo e lê; o miss popula o cache.

Invalidação do cache: limpeza total nos pontos de gc de buffers (avanço de `commit_generation` e
refresh pós-pull — o HEAD mudou); remoção por inode em `unlink`, `write`, `truncate` e rename de
arquivo. `create` e rename de arquivo populam um buffer não-dirty com o conteúdo corrente — sem isso,
ler o path novo antes do commit caía num blob inexistente no HEAD e retornava `EIO`.

---

## Limitações

1. **TTL de 1s** no kernel mascara mudanças vindas do pull por até 1 segundo.
2. **Sem FIFOs / hardlinks reais** (`link` é cópia). Symlinks são suportados: alvo opaco, nunca resolvido (no-follow); alvo absoluto quebra em outra máquina, como no Git puro.
3. **Permissões e xattrs em memória** — `chmod` aceito mas não persiste entre montagens; `chown` ignorado.
4. **mtime derivado do histórico** — arquivo não tocado na sessão expõe o timestamp do último commit que tocou o path (revwalk único com teto de 4096 commits; resíduo cai no timestamp do HEAD). Toques da sessão (flush/setattr) têm precedência e não persistem entre montagens. `atime`/`ctime`/`crtime` espelham o mtime disponível.
5. **Escritas não-commitadas são perdidas em crash** antes do debounce drenar.
6. **Swap implícito exige exatamente um arquivo-filho** no diretório de origem — ambiguidade retorna `EXDEV` e o app degrada para o fluxo de cópia.
7. **Cache de blobs não cobre arquivos maiores que o orçamento** (`read_cache_mb`) — leituras deles vão ao repo a cada syscall, mas por um handle persistente (uma abertura por mount, não uma por syscall).
8. **Write buffers acima de `large_file_threshold_mb` vivem em spool** (`.git/drive-sync-spool/`) com acesso randômico — pico de RAM limitado ao limiar; snapshot da fila é aplicado no drain com `rename`.
9. **Arquivos ignorados pelo `.gitignore` do repo são local-only visíveis** — aparecem no mount, persistem no working tree entre remounts e nunca entram em commit/push.
10. **Repo cifrado é transparente** — com `encryption_enabled`, índices nascem dos nomes decifrados, leituras abrem AEAD (chave errada/blob adulterado = `EIO`), readlink apresenta alvo claro e conflicted copies mostram o prefixo decifrado. Ignore/local-only ficam desativados nesse modo.
11. **Arquivos chunkados são transparentes** — path com manifest expõe tamanho real e leitura remontada por chunk (sha256 verificado, mismatch = `EIO`); `.chunks/` é invisível no mount.

---

## Como adicionar uma nova operação FUSE

1. Escrever teste em `tests/fs_hierarchy.rs` chamando `fs.apply_<op>(...)`.
2. Ver RED (função não existe).
3. Implementar `apply_<op>` em `src/fs.rs` como função pura sobre os índices/buffers.
4. Implementar o wrapper do trait `Filesystem` que delega e responde ao kernel.
5. Decidir o efeito no sync (`queue_*`) e no working tree.
6. Atualizar a tabela de operações acima.

## Como adicionar uma nova variante de `SyncOp`

Ver [fluxo-de-sincronizacao.md](./fluxo-de-sincronizacao.md#como-adicionar-uma-ação-pós-flush) para guia geral e [arquitetura.md](./arquitetura.md#como-adicionar-uma-nova-operação-de-mutação) para checklist end-to-end.

---

## Referências

| Arquivo | Responsabilidade |
| --- | --- |
| `src/fs.rs` | `GitDriveFS`, `FileEntry`, `DirEntry`, `apply_*`, write buffers + dirty, `refresh_if_needed`, impl `Filesystem` |
| `src/git.rs` | Leitura de blobs e listagem de tree |
| `src/sync.rs` | Consumidor de `queue_*` chamado pelas mutações FUSE |
| `tests/fs_hierarchy.rs` | Testes das funções `apply_*`, do modelo hierárquico e do ciclo do buffer |

Docs relacionadas:

- [arquitetura.md](./arquitetura.md)
- [fluxo-de-sincronizacao.md](./fluxo-de-sincronizacao.md)
