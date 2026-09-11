# Grafemas curtos em buffer de pilha — Linux, 11/09/2026

O CI pareado observou ganho de **3,29% em Unicode** e perda de **1,32% em ANSI**, com ANSI mais lento nos cinco pares. O resultado apoia o ganho Unicode neste ambiente, acompanhado dessa regressão; não demonstra melhora geral.

Comparação de `c6883b0db2f51a9d237e4eee200f71b64ab656b5` para `34ab430682a81ec0a91ee3bae740daeb91b2fefc`, com harness fixado na segunda revisão. A única alteração entre elas está em `src/grid/mod.rs`: a extensão de grafemas de até 64 bytes usa um buffer UTF-8 local antes de criar o `Arc<str>`; grafemas maiores mantêm o caminho com `String`, sem truncamento. Escalares isolados permanecem fora desse helper.

| Carga | Antes, MiB/s | Depois, MiB/s | Variação das medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 74,744 | 75,467 | +0,97% | 1/5 |
| ANSI | 68,955 | 68,047 | −1,32% | 5/5 |
| Unicode | 22,817 | 23,568 | +3,29% | 0/5 |
| Linhas curtas | 31,301 | 31,254 | −0,15% | 1/5 |

Variações de throughput de cada par, na ordem registrada:

| Carga | Par 1 | Par 2 | Par 3 | Par 4 | Par 5 | Mediana pareada |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| ASCII | +0,96% | +1,55% | −1,95% | +1,17% | +1,82% | +1,17% |
| ANSI | −0,74% | −1,41% | −1,55% | −0,81% | −1,07% | −1,07% |
| Unicode | +3,24% | +3,34% | +2,36% | +3,36% | +3,63% | +3,34% |
| Linhas curtas | +0,42% | −0,49% | +0,05% | +0,98% | +0,97% | +0,42% |

A razão entre medianas e a mediana das razões pareadas são cálculos distintos; por isso, linhas curtas apresenta sinais diferentes. Todas as amostras, mínimos, máximos, MAD, tempos de CPU e 300 observações de RTT estão no [relatório bruto](ci-alternate-report.json) e na [auditoria](alternate/audit.json). Unicode passou de 22,663–23,025 para 23,424–23,660 MiB/s. As faixas das outras cargas se sobrepõem; isso não elimina as perdas observadas nem prova ruído.

A mediana do tempo de CPU do terminal em Unicode caiu de 1,35 para 1,31 s. O RTT de protocolo mediano passou de 54,732 para 49,988 µs, com 150 observações por revisão. São contadores de CPU e respostas DSR, não latência visual ou prova de equivalência de renderização.

## Ambiente e método

[Execução 34623517573](https://github.com/guicybercode/kokuban.rs/actions/runs/34623517573), concluída com sucesso, em Ubuntu 24.04, **AMD EPYC 7763 64-Core Processor**, quatro CPUs lógicas disponíveis ao runner. O harness registrou afinidade `[0]`; a afinidade é herdada pelos processos filhos, sem leitura individual de cada terminal. Rust 1.94.1, Wayland nativo em Weston headless/Pixman, sem `DISPLAY`, DejaVu Sans Mono 14 px, tela alternativa sem histórico, 80×24 células e geometria 720×408 px, com 1 s de estabilização.

Foram cinco pares alternados antes/depois, depois/antes, antes/depois, depois/antes, antes/depois: **10 processos de terminal, 40 cargas e 300 RTTs**. Cada carga solicitou 32 MiB; os tamanhos efetivos e hashes estão na auditoria. Todos os processos preservaram geometria, configuração, payloads e a resposta DSR final esperada.

As revisões foram extraídas por `git archive` e compiladas sequencialmente com `cargo build --release --locked`, em diretórios de fonte e target separados, `CARGO_INCREMENTAL=0` e `CARGO_PROFILE_RELEASE_DEBUG=0`. O [trecho do log](alternate/ci-log-excerpt.txt) registra a compilação da aplicação nos dois diretórios e a ordem das medições. As árvores de dependências coincidem após normalizar apenas o caminho da aplicação.

## Proveniência e retenção

O [manifesto](manifest.json) relaciona os arquivos e limites da evidência. O relatório bruto é byte a byte idêntico ao download. A auditoria conferiu os 87 arquivos do artefato original e cada `case.json`, `sample.json` e `result.json` contra o relatório; preserva seus hashes, as configurações e os dez logs de terminal. Os scripts têm revisão e SHA-256 fixados; os payloads excluídos do upload foram regenerados a partir do gerador dessa revisão, com tamanhos e hashes conferidos.

`Cargo.toml`, `Cargo.lock` e o workflow são referenciados no [pacote circular-rows](../2026-09-11-circular-rows/manifest.json) após comparação byte a byte com as revisões exatas e, para Cargo, também com os arquivos do artefato. Árvores de dependências já retidas são igualmente reutilizadas somente quando idênticas.

Os hashes dos binários e do ZIP são **observações registradas pelo CI**: seus bytes não estão neste pacote e não foram recalculados independentemente. O inventário dos arquivos efetivamente retidos está em [evidence-sha256.txt](evidence-sha256.txt). O log integral baixado tem tamanho e hash registrados; somente trechos relevantes são retidos aqui.

## Validação e alcance

[CI de código 34623498103](https://github.com/guicybercode/kokuban.rs/actions/runs/34623498103), na mesma revisão: check/test/clippy passaram nas duas plataformas; 801 testes Rust no macOS e 899 no Linux, com um ignorado no Linux, além de 23 testes do harness por plataforma. Os smokes Linux aplicáveis também passaram. A validação local macOS em release passou os mesmos gates e 801 testes. Os comandos emitiram warnings; os [resumos e hashes dos logs](test-summary.json) não fazem afirmação de ausência de warnings.

Os testes adicionados cobrem resultados UTF-8 de 63/64/65 bytes, escalares de quatro bytes, grafemas válidos maiores que 64 bytes, fragmentação, snapshots de `Arc`, estilo e wrap pendente.

Este ensaio mede processamento PTY/DSR em CI headless. Não mede input-to-photon, equivalência visual, GPU física, uma sessão Omarchy/Hyprland ou desempenho relativo a outros terminais. Cinco pares descrevem esta execução; não autorizam extrapolação universal, nem comparação absoluta com ensaios em CPUs diferentes. A regressão ANSI permanece registrada.
