A comparação Linux de `5d1712e` com `5c1f436` **não demonstra ganho geral
do classificador de bytes ASCII**. O [run pareado 34620206592](https://github.com/guicybercode/kokuban.rs/actions/runs/34620206592)
terminou com sucesso; esse status confirma a execução válida, não uma melhora.

| Carga | Antes, mediana MiB/s | Depois, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 133,568 | 131,059 | −1,88% | 4/5 |
| ANSI | 114,461 | 112,916 | −1,35% | 4/5 |
| Unicode | 40,076 | 39,406 | −1,67% | 3/5 |
| Linhas curtas | 53,176 | 53,572 | +0,75% | 1/5 |

Foram cinco pares em ordem AB/BA/AB/BA/AB, com aproximadamente 32 MiB por
carga, tela alternativa sem histórico e afinidade herdada da CPU 0. O runner
x86_64 expôs quatro CPUs de um AMD EPYC 9V45, com Weston headless/Pixman,
Wayland e DejaVu Sans Mono 14. As dez instâncias mantiveram 80×24 células,
720×408 pixels e células de 9×17 pixels. O run anterior de UTF-8 usou EPYC
7763; seus valores absolutos não são comparáveis aos deste runner.

As variações por par, em ordem, foram:

| Carga | Par 1 | Par 2 | Par 3 | Par 4 | Par 5 |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | −5,45% | −0,56% | +0,52% | −3,70% | −2,13% |
| ANSI | −1,59% | −0,92% | +0,89% | −2,48% | −0,99% |
| Unicode | −0,98% | −1,82% | +2,33% | +1,17% | −1,67% |
| Linhas curtas | +3,58% | +12,40% | +0,75% | +1,46% | −4,22% |

Todos os intervalos mínimo–máximo se sobrepõem. Isso não prova ruído nem
equivalência. As medianas de CPU permaneceram em 0,21/0,25/0,77/0,56 segundos,
respectivamente; os contadores grosseiros não demonstram custo igual. O RTT
DSR mediano passou de 36,08 para 36,57 µs. Esses dados não medem latência até
o pixel, equivalência visual ou desempenho em Omarchy físico.

O [manifesto](manifest.json) registra distribuições, builds das revisões exatas
em diretórios isolados, scripts fixados no Git, configurações e payloads
reproduzidos. A auditoria conferiu 10 processos, 40 cargas e 300 RTT. O
[relatório bruto](ci-report.json) é byte a byte igual ao download. Hashes dos
binários e do ZIP são observações do CI: seus bytes não foram retidos para
rehash independente. [artifact-sha256.tsv](artifact-sha256.tsv) inventaria os
87 arquivos baixados; [evidence-sha256.txt](evidence-sha256.txt) cobre este
pacote retido. Inputs Cargo e workflow idênticos referenciam o
[pacote anterior](../2026-09-11-utf8-dispatch/manifest.json), cujas perdas
continuam preservadas.

O [resumo de validação](test-summary.json) registra 785 testes locais em
release. O CI direto de `5c1f436` foi cancelado; o [CI 34620228850](https://github.com/guicybercode/kokuban.rs/actions/runs/34620228850)
de `e46f8f6`, com os mesmos inputs da aplicação, passou em Linux e macOS:
883 testes Rust no Linux, com um ignorado, e 785 no macOS; mais 23 testes do
harness em cada sistema. Check e Clippy passaram com avisos registrados.
