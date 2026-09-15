# Perfil release com LTO: evidências de 2026-09-15

`[profile.release]` com `lto = "fat"` e `codegen-units = 1` (sem `panic = "abort"`)
melhorou o throughput do PTY no Linux x86_64 em **+35,3%** (ASCII), **+30,6%**
(Unicode), **+9,6%** (ANSI) e **+5,2%** (linhas curtas), sem par mais lento, e
reduziu o tempo de lookup de glifos em até 72%. Custos: build release limpo
26,7 s → 41,9 s no Apple M4, repaint completo de frame ~+1–2% mais lento no x86
e grafemas +2–4% no Linux. O binário macOS arm64 caiu de 5,1 MB para 4,3 MB.

Fontes: antes `eff127f1611104602503110ca279dc65b0bf01eb`, depois
`a0cd4c2a76098d00e5e67c70c4f436ee6468543a` (só a mudança no `Cargo.toml`).
Harness dos workflows: main `2140020`.

## CI (workflow_dispatch, runners do GitHub)

PTY — [run 35005113581](https://github.com/guicybercode/kokuban.rs/actions/runs/35005113581),
Ubuntu 24.04, EPYC 7763, Weston headless, 5 pares AB/BA, 32 MiB, tela alternativa:

| Workload | Antes (MiB/s) | Depois (MiB/s) | Por par (mediana, faixa) | Mais lentos |
|---|---:|---:|---|---:|
| ascii | 81,2 | 108,5 | +35,3% (+25,7 … +38,5) | 0/5 |
| ansi | 70,1 | 77,1 | +9,6% (+9,3 … +13,2) | 0/5 |
| unicode | 40,6 | 52,9 | +30,6% (+24,6 … +32,8) | 0/5 |
| short_lines | 42,2 | 44,4 | +5,2% (+2,9 … +7,4) | 0/5 |

Controle A/A do binário antigo ([run 35005116536](https://github.com/guicybercode/kokuban.rs/actions/runs/35005116536)):
−7,7% … +9,2%, medianas −3,9% … +0,9%.

Repaint de CPU — [run 35005119684](https://github.com/guicybercode/kokuban.rs/actions/runs/35005119684),
6 pares, variação mediana de latência:

| Cenário | x86_64 | arm64 |
|---|---|---|
| linha única, incremental | −3,4% (ascii), −1,6% (unicode) | −4,9% (ascii), −3,1% (unicode) |
| linha única, completo | +1,1 … +1,9% | +0,2% (ascii), −1,8% (unicode) |
| tela cheia, incremental | +1,6 … +1,9% | +0,2% (ascii), −2,2% (unicode) |
| tela cheia, completo | +1,2 … +1,7% | +0,1% (ascii), −2,0% (unicode) |

No x86_64 os cenários de repaint completo e tela cheia ficaram mais lentos em
5–6 de 6 pares; é a regressão aceita desta mudança.

Lookup de glifos aquecido — [run 35005122582](https://github.com/guicybercode/kokuban.rs/actions/runs/35005122582),
6 pares, variação mediana de tempo por lookup:

| Workload | macOS | Linux arm64 | Linux x86_64 |
|---|---|---|---|
| ascii-regular | −55,2% | −64,4% | −0,6% |
| ascii-four-styles | −50,8% | −65,1% | +0,2% |
| mixed-unicode | −71,7% | −60,1% | −37,0% |
| unicode-scalars | −72,1% | −47,6% | −48,5% |
| graphemes | −1,8% | +3,7% (6/6 mais lentos) | +2,4% (6/6 mais lentos) |

Relatórios de repaint e glifos estão comprimidos (`ci/*.json.gz`).

## macOS local (Apple M4, macOS 26.2)

- **Decoder isolado** (`examples/terminal-throughput 16 5`, 6 pares,
  `macos-local/decoder-ab.json`): ascii +10,2%, short-lines +12,5%, ansi +3,7%,
  unicode +16,8%, 0/6 mais lentos; A/A com medianas −0,4% … +0,4%.
- **PTY de ponta a ponta com o leitor antigo (sleep de 2 ms)**
  (`pty-ab-sleep-reader-report.json`): LTO **mais lento** em 4–5 de 5 pares
  (ascii −19,3%, ansi −16,1%, unicode −16,4%, short_lines −8,5%). O leitor
  antigo dormia sempre que esvaziava o PTY; código mais rápido esvaziava antes e
  dormia mais. O runner rejeitou esse relatório por geometria diferente entre
  pares; as variações acima usam só pares com a mesma grade nos dois lados.
- **PTY de ponta a ponta com o leitor com poll** (main `9058b16` com e sem o
  perfil via `CARGO_PROFILE_RELEASE_*`, `pty-ab-report.json`, runner aprovado):
  ascii 135,6 → 157,2 MiB/s (+16,5%), ansi +4,0%, unicode +8,4%, short_lines
  +8,4%; o par 2 ficou mais lento em três workloads. O A/A desse binário antes
  do LTO ficou em −5,1% … +6,8% ([leitor com poll](../../macos-evidence/2026-09-15-poll-reader/README.md)).

## Limitações

- Os workflows de CI mediram `eff127f`, anterior ao leitor com poll do macOS;
  o Linux não usa esse leitor, então os números Linux continuam válidos.
- A medição macOS com poll aplicou o perfil por variáveis de ambiente sobre
  `9058b16`, equivalente ao `Cargo.toml` deste commit, mas não o commit final.
- A máquina macOS estava em uso e o AeroSpace redimensionou algumas janelas.
