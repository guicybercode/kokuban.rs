# Experimento rejeitado: reuso de linhas do histórico

**A otimização foi rejeitada para a versão final após a regressão medida
no CI x86.** Este pacote preserva o candidato e os resultados, sem tratar
a redução de alocações como melhoria geral. O revert é
`4b05e0807e269fd83c38dbef5644e8d71ccee955`.

A revisão `794d03dd5a2679f95b3c3c588e2616bee2a04b52` reutiliza o buffer da
linha mais antiga descartada pelo histórico. A referência é
`4d382d8f6d515a4b54dcd82c1c4df3e035ba7590`; as únicas diferenças de fonte
estão em `src/grid/mod.rs` e `src/grid/buffer.rs`, incluindo seus testes.
As alterações de tema Omarchy não entram na comparação.

O teste isolado macOS passou de **10.000 para zero chamadas de alocação**,
mas a medição Linux x86 da tela principal mostrou **regressão de 5,57% em
linhas curtas, com perda nos cinco pares**. Reduzir alocações não demonstra
por si só uma melhoria de desempenho do terminal.

## Medições Linux públicas

O [run principal 34718883607](https://github.com/guicybercode/kokuban.rs/actions/runs/34718883607)
usou AMD EPYC 7763, Ubuntu 24.04 x86_64, Rust 1.94.1 release, Weston 13
headless/Pixman e DejaVu Sans Mono 14. O histórico permite 10.000 linhas.
Foram cinco pares AB/BA/AB/BA/AB, aproximadamente 32 MiB por carga,
afinidade do executor na CPU 0, grade 80×24 e 720×408 pixels.

| Carga | Antes, mediana MiB/s | Depois, mediana MiB/s | Variação entre medianas | Mediana da variação por par | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 54,954 | 55,782 | +1,51% | +0,31% | 2/5 |
| ANSI | 48,046 | 48,303 | +0,53% | +0,43% | 2/5 |
| Unicode | 26,959 | 26,018 | −3,49% | −2,19% | 4/5 |
| Linhas curtas | 7,203 | 6,802 | −5,57% | −5,36% | 5/5 |

A vazão de linhas curtas passou de 7,140–7,358 para 6,781–6,820 MiB/s; as faixas
não se sobrepõem. As variações por par foram −7,84%, −5,87%, −5,28%,
−5,36% e −4,49%. As faixas das outras cargas se sobrepõem; isso não elimina
as perdas nem demonstra equivalência. O
[relatório bruto principal](ci-primary-report.json) preserva as amostras,
CPU, RTT e configurações; o [manifesto](manifest.json) mantém as
estatísticas recalculadas, incluindo razões por par.

O [run alternativo 34718885163](https://github.com/guicybercode/kokuban.rs/actions/runs/34718885163)
usou AMD EPYC 9V74, tela alternativa sem histórico e os mesmos parâmetros
de amostragem, fonte e geometria:

| Carga | Antes, mediana MiB/s | Depois, mediana MiB/s | Variação entre medianas | Mediana da variação por par | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 93,277 | 93,593 | +0,34% | +0,26% | 1/5 |
| ANSI | 84,666 | 85,551 | +1,05% | +1,64% | 0/5 |
| Unicode | 42,357 | 42,630 | +0,64% | +0,82% | 0/5 |
| Linhas curtas | 40,718 | 41,097 | +0,93% | +0,24% | 2/5 |

As faixas das quatro cargas se sobrepõem. Os resultados pequenos e
positivos desse perfil não anulam a regressão da tela principal.
Os runners são diferentes: compare as revisões dentro de cada run;
a diferença entre perfis não isola o efeito do histórico. O
[relatório alternativo bruto](ci-alternate-report.json) mantém todas as
amostras. Ao todo, a auditoria conferiu 20 processos, 80 cargas e 600 RTTs.

Os builds do CI extraem as revisões exatas por `git archive` e usam fontes
e targets separados. A auditoria compara o digest do ZIP com o GitHub,
confere `case.json`, `sample.json`, `result.json`, configurações e logs,
regenera os payloads e recalcula as estatísticas. Arquivos Cargo, árvores
de dependências e scripts iguais já rastreados são referenciados por hash
e revisão. Os executáveis não foram retidos para rehash independente.

## Teste de alocação macOS

O [fixture](allocations/history-allocations.rs) cria uma grade 80×24 com
histórico de 1.000 linhas e escreve 1.100 linhas antes de ligar os contadores.
Depois mede 10.000 linhas `x`, com retorno de carro e nova linha. O histórico
está cheio antes e depois do trecho medido. Não há janela ou renderizador.

| Contador no trecho medido | Antes | Depois |
| --- | ---: | ---: |
| Chamadas `alloc`/`realloc` | 10.000 | 0 |
| Soma dos tamanhos solicitados, bytes | 32.000.000 | 0 |

Esses bytes são solicitações cumulativas, sem subtrair desalocações.
**Não representam redução de RSS ou de memória retida**, nem o total de
alocações da aplicação gráfica. Os logs
[anterior](allocations/allocations-before.log) e
[atual](allocations/allocations-v2.log) registram compilação e resultado.
Os 646 arquivos rastreados de cada fonte local coincidem com o Git das
revisões comparadas. O exemplo temporário do candidato já tinha sido
removido no momento da auditoria; o fixture separado e seu log foram retidos.

Para repetir, coloque o fixture em `examples/history-allocations.rs` de
cada revisão e execute `cargo run --release --locked --example history-allocations`.
Use targets separados por revisão para impedir reutilização de um binário
antigo. O [manifesto de alocação](allocations/manifest.json) registra a
origem dos arquivos, os contadores e os limites do teste.

## Validação e limites

Os testes adicionados cobrem reutilização do mesmo buffer, estilos,
grafemas, cursores salvos, múltiplas evicções, rolagem de várias linhas,
histórico insuficiente e liberação de referências/capacidade antigas.
Check, testes e Clippy passaram localmente no macOS: 594 testes do
executável e 231 do exemplo, com avisos registrados no
[resumo dos logs](macos-validation.json).

Uma medição Linux local foi omitida: erro de entrada/saída do Docker
impediu preservar seus relatórios brutos. Seus tempos não sustentam as
conclusões deste pacote. Os resultados Linux publicados acima provêm dos
artefatos públicos do CI. O teste de alocação macOS permanece separado.

Os cinco pares descrevem cada execução e não estabelecem ganho universal
ou significância estatística. DSR verifica a resposta após um marcador,
sem conferir texto ou pixels. Permanecem fora deste ensaio apresentação,
teclado até a tela, uma sessão física Omarchy/Hyprland com GPU e monitor
e comparação com terminais concorrentes.
