# Medições Linux em release

O workflow manual `Linux release resource measurements` compila com Rust 1.94.1 e `cargo build --release --locked`, sem strip ou LTO adicionais. Ele registra manifesto, lockfile, árvore Cargo, versões de pacotes, dependências ELF, tamanho/hash do binário e hardware do runner. O perfil de release mantém `debug=0`.

Para comparar duas revisões no mesmo runner Linux, use o workflow manual
`Linux paired revision measurements`:

```sh
gh workflow run linux-paired-performance.yml \
  -f before=a27b8ce -f after=4bcf68c -f screen=alternate
```

Ele resolve os commits, compila os dois arquivos-fonte Git com Rust 1.94.1 e
lockfile, registra os hashes dos executáveis e executa cinco pares alternados
com cargas de 32 MiB na mesma CPU permitida. A sessão usa Ubuntu 24.04, Weston headless
pixman, DejaVu Sans Mono 14 e grade 80×24. `screen=primary` habilita histórico
de 10.000 linhas; `alternate` usa a tela alternativa sem histórico. O artefato
retido por sete dias contém JSONs, configurações, logs, hashes e proveniência,
inclusive em caso de falha. Os payloads binários são omitidos do upload;
seus tamanhos e hashes permitem verificar a reprodução pelo gerador do script.

Em uma sessão Wayland Linux existente, o mesmo executor aceita dois binários
previamente compilados. Os rótulos de revisão são declarados pelo operador.
Hashes de executáveis idênticos são rejeitados; `--allow-identical-binaries`
permite um controle A/A explícito, identificado como medição de variabilidade:

```sh
python3 scripts/compare-kokuban-revisions.py \
  --before /caminho/anterior --after /caminho/atual \
  --before-ref REVISAO_ANTERIOR --after-ref REVISAO_ATUAL \
  --artifacts-dir /tmp/kokuban-paired --backend wayland
```

## Reuso de linhas do histórico: alocações e regressão em 2026-09-12

A revisão `794d03d` reutiliza a alocação de uma linha descartada quando o
histórico está cheio. **A otimização foi rejeitada para a versão final
após a regressão medida abaixo.** Contra `4d382d8`, um fixture headless macOS passou de
10.000 para zero chamadas `alloc`/`realloc` ao escrever 10.000 linhas numa
grade de 80 colunas, após aquecer o histórico de 1.000 linhas. As solicitações
cumulativas passaram de 32.000.000 para zero bytes; **isso não mede RSS
nem redução de memória retida**.

O [ensaio Linux público com histórico de 10.000 linhas](https://github.com/guicybercode/kokuban.rs/actions/runs/34718883607)
encontrou **regressão de 5,57% em linhas curtas, com perda nos cinco pares**:

| Carga | Antes, mediana MiB/s | Depois, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 54,954 | 55,782 | +1,51% | 2/5 |
| ANSI | 48,046 | 48,303 | +0,53% | 2/5 |
| Unicode | 26,959 | 26,018 | −3,49% | 4/5 |
| Linhas curtas | 7,203 | 6,802 | −5,57% | 5/5 |

A vazão de linhas curtas caiu de 7,140–7,358 para 6,781–6,820 MiB/s, sem sobreposição
das faixas. A mediana da variação por par foi −5,36%, um cálculo distinto
da razão entre medianas. O run usou AMD EPYC 7763, Ubuntu 24.04 x86_64,
Rust 1.94.1 release, Weston 13 headless/Pixman, afinidade na CPU 0 e
cinco pares alternados de aproximadamente 32 MiB por carga. Todas as
instâncias mantiveram 80×24 células e 720×408 pixels.

Na [tela alternativa, sem histórico](https://github.com/guicybercode/kokuban.rs/actions/runs/34718885163),
o runner foi AMD EPYC 9V74, com os mesmos parâmetros de amostragem e geometria:

| Carga | Antes, mediana MiB/s | Depois, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 93,277 | 93,593 | +0,34% | 1/5 |
| ANSI | 84,666 | 85,551 | +1,05% | 0/5 |
| Unicode | 42,357 | 42,630 | +0,64% | 0/5 |
| Linhas curtas | 40,718 | 41,097 | +0,93% | 2/5 |

As faixas se sobrepõem nas quatro cargas. Os perfis usam CPUs diferentes:
suas diferenças não podem ser atribuídas apenas ao histórico. O resultado
alternativo não anula a regressão principal; o revert está registrado em
`4b05e0807e269fd83c38dbef5644e8d71ccee955`.

O [pacote auditado](linux-evidence/2026-09-12-history-reuse/README.md)
preserva os relatórios públicos, as perdas e a proveniência dos builds
separados, além do fixture e dos logs de alocação macOS. Somente os dois
arquivos do grid diferem entre as revisões; mudanças de tema não entram.
A redução de alocações não demonstrou ganho geral de vazão. O escopo
continua sendo processamento PTY/DSR e contadores do teste isolado, sem
medir apresentação, teclado até a tela ou uso físico no Omarchy.

## Fronteiras Unicode por páginas: dois perfis em 2026-09-11

A revisão `8dafe9e` classifica casos simples de fronteira de grafema com
páginas Unicode verificadas, mantendo o segmentador completo para os casos
que precisam de contexto. Contra `34ab430`, **Unicode ganhou 41,26% na tela
alternativa e 35,63% na principal**, com melhora nos cinco pares de cada run.

Na [tela alternativa, sem histórico](https://github.com/guicybercode/kokuban.rs/actions/runs/34626144701):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 75,206 | 76,463 | +1,67% | 2/5 |
| ANSI | 66,858 | 67,928 | +1,60% | 1/5 |
| Unicode | 23,007 | 32,499 | +41,26% | 0/5 |
| Linhas curtas | 30,585 | 30,716 | +0,43% | 2/5 |

Na [tela principal, com histórico de 10.000 linhas](https://github.com/guicybercode/kokuban.rs/actions/runs/34626168671):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 61,626 | 61,297 | −0,53% | 4/5 |
| ANSI | 63,892 | 64,034 | +0,22% | 2/5 |
| Unicode | 24,523 | 33,260 | +35,63% | 0/5 |
| Linhas curtas | 12,848 | 12,819 | −0,23% | 3/5 |

As perdas de ASCII e linhas curtas na tela principal permanecem registradas.
Na alternativa, ASCII caiu 8,30% no quinto par, apesar da mediana maior.
Unicode passou de 22,052–23,045 para 32,242–32,721 MiB/s na alternativa
e de 24,082–24,775 para 32,544–33,792 na principal. As faixas das demais
cargas se sobrepõem; isso não demonstra equivalência nem elimina as perdas.
As razões por par, mínimos, máximos e MAD estão no
[pacote auditado](linux-evidence/2026-09-11-boundary-pairs/README.md).

Cada perfil usou cinco pares AB/BA/AB/BA/AB, cerca de 32 MiB por carga,
Ubuntu 24.04, Rust 1.94.1 release, Weston 13 headless com Pixman e DejaVu
Sans Mono 14. A afinidade do executor foi `[0]`, herdada pelos filhos.
O runner alternativo foi AMD EPYC 7763; o principal, EPYC 9V74. Compare
antes/depois dentro de cada run: a diferença entre os perfis não isola
o efeito do histórico. As vinte instâncias mantiveram 80×24 células e
720×408 pixels.

Ambos os workflows passaram. A auditoria conferiu 20 processos, 80 cargas,
600 RTTs, payloads reproduzidos e builds separados das revisões exatas.
O [manifesto](linux-evidence/2026-09-11-boundary-pairs/manifest.json) relaciona
os relatórios brutos, idênticos aos artefatos oficiais; arquivos Cargo, árvores de dependências
e scripts iguais já retidos são referenciados por hash. Os binários não
foram retidos para rehash independente. Esses workflows testam o executor
Python e compilam as duas revisões; não executam testes Rust. Os resultados
cobrem processamento PTY/DSR, sem medir fidelidade visual, apresentação,
teclado até a tela ou uma sessão física Omarchy/Hyprland com GPU e monitor.

Um [experimento local posterior de rejeição antecipada](linux-evidence/2026-09-11-scalar-shortcut-experiment/README.md)
ficou fora da implementação: apresentou resultados mistos e comparou binários
que também diferiam na rota de clipboard, sem isolar o efeito da otimização.
O pacote preserva o patch e as observações para orientar uma medição futura.

## Classificação de controles ASCII: resultado misto em 2026-09-11

A revisão `5c1f436` separa controles ASCII antes da busca de texto imprimível
e da seleção de comprimento UTF-8. A comparação com `5d1712e` no
[run `34620206592`](https://github.com/guicybercode/kokuban.rs/actions/runs/34620206592)
**não demonstrou ganho geral**:

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 133,568 | 131,059 | −1,88% | 4/5 |
| ANSI | 114,461 | 112,916 | −1,35% | 4/5 |
| Unicode | 40,076 | 39,406 | −1,67% | 3/5 |
| Linhas curtas | 53,176 | 53,572 | +0,75% | 1/5 |

Foram cinco pares AB/BA/AB/BA/AB, tela alternativa sem histórico e cerca de
32 MiB por carga. O runner usou AMD EPYC 9V45, com quatro CPUs expostas e
afinidade na CPU 0, Weston headless com Pixman e DejaVu Sans Mono 14.
As dez instâncias mantiveram 80×24 células e 720×408 pixels. O run anterior
do parser UTF-8 usou EPYC 7763; os valores absolutos entre esses runners
não são comparáveis e não comprovam recuperação da regressão anterior.

Todos os intervalos mínimo–máximo se sobrepõem, o que não prova ruído ou
equivalência. O [pacote auditado](linux-evidence/2026-09-11-ascii-controls/README.md)
preserva as perdas, razões por par, CPU, RTT, relatório bruto e proveniência
dos builds separados. Foram conferidos 10 processos, 40 cargas e 300 RTT.
Os testes locais e o CI de uma revisão com código equivalente passaram;
o cancelamento do CI direto também está registrado no pacote. Esses testes
verificam comportamento; o desempenho é avaliado pelas medições acima.
O escopo continua sendo
processamento e DSR, sem comparação visual ou medição em Omarchy físico.

## Grafemas curtos na pilha: ganho Unicode e perda ANSI em 2026-09-11

A revisão `34ab430` usa um buffer local de 64 bytes ao estender grafemas
curtos, evitando uma alocação temporária antes de criar o texto compartilhado.
Grafemas maiores mantêm o caminho com `String`, sem truncamento. No
[run pareado `34623517573`](https://github.com/guicybercode/kokuban.rs/actions/runs/34623517573),
contra `c6883b0`, **Unicode ganhou 3,29%, enquanto ANSI perdeu 1,32%**:

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 74,744 | 75,467 | +0,97% |
| ANSI | 68,955 | 68,047 | −1,32% |
| Unicode | 22,817 | 23,568 | +3,29% |
| Linhas curtas | 31,301 | 31,254 | −0,15% |

Unicode melhorou nos cinco pares, passando de 22,663–23,025 para
23,424–23,660 MiB/s. ANSI piorou nos cinco pares. Linhas curtas teve uma
perda entre medianas, mas a mediana das razões por par foi +0,42%; os dois
cálculos permanecem separados no
[pacote auditado](linux-evidence/2026-09-11-stack-graphemes/README.md).

O ensaio usou Ubuntu 24.04, AMD EPYC 7763, afinidade na CPU 0, Rust 1.94.1,
Weston headless com Pixman e DejaVu Sans Mono 14. Foram cinco pares de
aproximadamente 32 MiB por carga, tela alternativa sem histórico, com
80×24 células e 720×408 pixels em todas as amostras. A auditoria conferiu
10 processos, 40 cargas, 300 RTTs e os builds das revisões exatas em
diretórios separados. Os hashes de executáveis foram registrados pelo CI;
seus bytes não foram retidos para rehash independente.

Os testes incluem os limites de 63/64/65 bytes, grafemas longos, entrada
fragmentada, estilos, snapshots e wrap pendente. `check`, testes e Clippy
passaram localmente e no [CI da mesma revisão](https://github.com/guicybercode/kokuban.rs/actions/runs/34623498103):
801 testes Rust no macOS e 899 no Linux, com um ignorado no Linux e avisos
registrados. O resultado de desempenho é misto; mede processamento PTY/DSR,
sem demonstrar ganho geral, equivalência visual ou desempenho em Omarchy físico.

## Reuso de larguras Unicode: resultados mistos em 2026-09-11

A revisão `c6883b0` reutiliza larguras já calculadas ao escrever escalares e
estender grafemas. Mantém a largura natural do glifo separada das células
ocupadas após o recorte pela grade e pela margem. A comparação com `5c1f436`
mostrou variações pequenas e resultados mistos: **linhas curtas variaram
+2,11% na tela alternativa e −2,27% na primária**, enquanto Unicode variou
+0,54% e +1,37%. Esses dados não demonstram uma melhoria geral.

Cada perfil executou cinco pares AB/BA, aproximadamente 32 MiB por carga,
com os dois processos na CPU 0 do mesmo runner durante sua execução.
A tela alternativa usou um AMD EPYC 7763; a primária, um AMD EPYC 9V74.
Os perfis são analisados separadamente: a diferença entre eles não pode
ser atribuída apenas ao histórico. Ambos usaram Ubuntu 24.04 x86_64,
Rust 1.94.1 release, Weston 13 headless com Pixman e DejaVu Sans Mono 14.
As vinte amostras mantiveram 80×24 células e 720×408 pixels.

Na [tela alternativa, sem histórico](https://github.com/guicybercode/kokuban.rs/actions/runs/34622759863):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 76,365 | 76,635 | +0,35% |
| ANSI | 67,441 | 68,260 | +1,21% |
| Unicode | 22,606 | 22,728 | +0,54% |
| Linhas curtas | 30,653 | 31,299 | +2,11% |

Linhas curtas melhoraram nos cinco pares. ANSI caiu em três pares, apesar
da mediana maior; a mediana das razões por par foi −0,51%, uma agregação
distinta da razão entre medianas da tabela. Para Unicode, essa mediana por
par foi apenas +0,006%, com perdas em dois pares. ASCII também caiu em dois.
O [relatório alternativo bruto](linux-evidence/2026-09-11-reused-widths/ci-alternate-report.json)
preserva os intervalos e todas as amostras.

Na [tela primária, com histórico de 10.000 linhas](https://github.com/guicybercode/kokuban.rs/actions/runs/34622836740):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 51,539 | 51,536 | −0,005% |
| ANSI | 51,153 | 50,774 | −0,74% |
| Unicode | 18,960 | 19,219 | +1,37% |
| Linhas curtas | 10,219 | 9,987 | −2,27% |

Linhas curtas perderam vazão em quatro pares, variando de
9,610–10,331 MiB/s antes para 9,881–10,005 MiB/s depois. ASCII caiu em três
pares, ANSI em dois e Unicode em um. Todas as cargas dos dois perfis
tiveram intervalos mínimo–máximo sobrepostos. As cinco amostras por lado
não estabelecem significância estatística. O
[relatório primário bruto](linux-evidence/2026-09-11-reused-widths/ci-primary-report.json)
mantém essas perdas junto dos ganhos observados.

Os dois workflows passaram. A auditoria conferiu 20 processos, 80 cargas,
600 observações de RTT, configurações, geometria, payloads reproduzidos e
scripts fixados no Git. Os dois builds de cada execução usaram arquivos
Git e diretórios de compilação separados. Os hashes dos executáveis foram
registrados no CI; seus bytes não foram retidos para rehash independente.
O [manifesto](linux-evidence/2026-09-11-reused-widths/manifest.json) preserva
a proveniência e referencia os manifestos Cargo, lockfile, árvores Cargo
e workflow já arquivados, após conferir sua igualdade byte a byte.

Os testes novos cobrem VS15/VS16, contração e crescimento de grafemas,
grade de uma coluna, margem sem wrap, reflow e rolagem com histórico.
`check`, testes e Clippy passaram em macOS com
`--release --locked --all-targets` (579 testes do executável e 216 do
exemplo), com avisos registrados. O
[CI Linux/macOS de `c6883b0`](https://github.com/guicybercode/kokuban.rs/actions/runs/34622706935)
também passou; o [resumo de validação](linux-evidence/2026-09-11-reused-widths/test-summary.json)
registra os resultados e hashes dos logs originais.

As medições cobrem processamento pelo PTY e resposta DSR. Não verificam
equivalência visual, apresentação de quadros ou superioridade sobre outros
terminais no Omarchy com GPU e monitor reais. Diagnósticos macOS de vazão
e a alteração posterior de alocação em `append_grapheme` não fazem parte
desses resultados Linux.

## Linhas circulares: tela alternativa e histórico em 2026-09-11

A revisão `5d1712e` mantém uma origem circular comum aos endereços das linhas,
aos sufixos uniformes e aos metadados. A rolagem da tela inteira atualiza essa
origem sem mover os três vetores; rolagens parciais normalizam a ordem antes
de alterar a região. O processamento continua limpando as linhas expostas.
Na comparação com `4614d9e`, **linhas curtas ganharam 38,82% na tela
alternativa e 1,50% na tela primária com histórico**. Unicode variou −0,39%
e +0,48%, respectivamente; o resultado depende da carga e do cenário.

Cada cenário executou cinco pares AB/BA de aproximadamente 32 MiB por carga,
no mesmo runner dentro de cada execução: Ubuntu 24.04 x86_64, AMD EPYC 7763,
afinidade na CPU 0, Rust 1.94.1 release, Weston 13 headless com Pixman e
DejaVu Sans Mono 14. Os dois runs passaram e foram auditados separadamente.
As vinte amostras mantiveram 80×24 células e 720×408 pixels.

Na [tela alternativa, sem histórico](https://github.com/guicybercode/kokuban.rs/actions/runs/34618962851):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 73,402 | 76,670 | +4,45% |
| ANSI | 63,361 | 66,327 | +4,68% |
| Unicode | 22,680 | 22,591 | −0,39% |
| Linhas curtas | 21,596 | 29,979 | +38,82% |

Linhas curtas melhoraram nos cinco pares, com intervalos de
21,183–21,645 MiB/s antes e 29,698–30,163 MiB/s depois. Unicode caiu em
quatro pares, com intervalos sobrepostos. O
[relatório alternativo bruto](linux-evidence/2026-09-11-circular-rows/ci-alternate-report.json)
preserva a variação de todas as amostras.

Na [tela primária, com histórico de 10.000 linhas](https://github.com/guicybercode/kokuban.rs/actions/runs/34618965511):

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 52,518 | 53,768 | +2,38% |
| ANSI | 46,119 | 46,490 | +0,81% |
| Unicode | 19,500 | 19,594 | +0,48% |
| Linhas curtas | 7,001 | 7,107 | +1,50% |

Com histórico, todas as cargas tiveram intervalos mínimo–máximo sobrepostos.
Unicode variou de 18,558 a 19,840 MiB/s antes e de 18,590 a 19,841 depois;
caiu em dois pares, e a mediana das razões por par foi apenas +0,005%.
ANSI também caiu em dois pares e ASCII em um. Linhas curtas melhoraram nos
cinco pares, mas sem reproduzir o ganho da tela alternativa. A mediana das
razões por par foi +3,44% nessa carga, distinta do +1,50% entre medianas.
O [relatório primário bruto](linux-evidence/2026-09-11-circular-rows/ci-primary-report.json)
contém os números Linux; diagnósticos macOS não entram nessa comparação.

A auditoria conferiu 20 processos, 80 cargas, 600 observações de RTT,
configurações, geometria, payloads reproduzidos e os três scripts fixados
no Git. Os dois builds de cada execução usaram as revisões exatas em
diretórios separados; somente `src/grid/buffer.rs` mudou entre os inputs
da aplicação. Os hashes
dos executáveis foram registrados no CI, mas seus bytes não foram retidos
para rehash independente. O
[manifesto](linux-evidence/2026-09-11-circular-rows/manifest.json) preserva a
proveniência, o workflow fixado e os avisos dos vinte logs dos terminais.

Os quatro testes novos cobrem voltas completas, rolagens parciais após
rolagens completas, metadados, dimensões vazias, índices inválidos e duração
dos grafemas compartilhados. `check`, testes e Clippy passaram em macOS
com `--release --locked --all-targets` (573 testes do executável e 210 do
exemplo), com avisos registrados. O
[CI Linux/macOS de `5d1712e`](https://github.com/guicybercode/kokuban.rs/actions/runs/34618903077)
também passou; o [resumo de validação](linux-evidence/2026-09-11-circular-rows/test-summary.json)
preserva os resultados e hashes dos logs originais.

Estas medições cobrem processamento pelo PTY e resposta DSR, sem verificar
equivalência visual ou apresentação de quadros. Não estabelecem
superioridade sobre outros terminais ou desempenho em uma máquina Omarchy
com GPU e monitor reais, e não incluem alterações posteriores do parser.

## Decodificação UTF-8 contígua: ganho e regressões em 2026-09-11

A revisão original `4614d9e` decodifica diretamente um escalar UTF-8 completo
quando todos os seus bytes estão disponíveis no estado normal do parser.
Sequências incompletas ou inválidas mantêm o processamento byte a byte.
A medição contra `48afe69` mostrou **ganho de 5,6% em Unicode, acompanhado de
quedas de 2,8% em ANSI e 7,7% em linhas curtas**. Este resultado contém uma
troca de desempenho entre cargas, sem demonstrar uma melhoria geral.

O [workflow pareado](https://github.com/guicybercode/kokuban.rs/actions/runs/34618310640)
executou cinco pares em ordem AB/BA no mesmo runner Ubuntu 24.04 x86_64,
AMD EPYC 7763, com afinidade na CPU 0. Ambos os builds usaram Rust 1.94.1
release e diretórios de compilação separados. Cada carga continha cerca de
32 MiB, na tela alternativa sem histórico, com DejaVu Sans Mono 14 e Weston
13 headless usando Pixman. As dez amostras mantiveram 80×24 células e
720×408 pixels, com configurações idênticas.

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s | Variação entre medianas |
| --- | ---: | ---: | ---: |
| ASCII | 73,634 | 73,303 | −0,45% |
| ANSI | 64,852 | 63,068 | −2,75% |
| Unicode | 21,582 | 22,800 | +5,64% |
| Linhas curtas | 23,351 | 21,546 | −7,73% |

Linhas curtas perderam vazão nos cinco pares; seus intervalos foram
22,773–24,053 MiB/s antes e 21,332–21,685 MiB/s depois. ANSI caiu em quatro
pares. Unicode caiu em um dos cinco pares, apesar da mediana maior.
A mediana das razões por par indica −8,65% em linhas curtas e −3,46% em ANSI;
esse cálculo difere da razão entre medianas apresentada na tabela.
O [relatório bruto](linux-evidence/2026-09-11-utf8-dispatch/ci-report.json)
preserva todas as amostras, intervalos e razões, sem incorporar revisões
posteriores do parser ou do buffer.

A auditoria conferiu os dez conjuntos de resultados e configurações,
40 cargas, 300 observações de RTT, hashes dos payloads reproduzidos e os
três scripts do executor contra o Git fixado. Os manifestos e lockfiles dos
builds correspondem às revisões declaradas. Os hashes distintos dos
executáveis foram registrados no CI; os bytes desses binários não foram
retidos no artefato para uma nova verificação independente. O
[manifesto](linux-evidence/2026-09-11-utf8-dispatch/manifest.json) registra essas
verificações, o workflow fixado e os avisos dos logs.

Os três testes diferenciais cobrem todos os pontos de divisão dos exemplos
UTF-8, limites de escalares, sequências inválidas, grafemas compostos, modos
do terminal e ordem dos eventos. `check`, testes e Clippy passaram em macOS
com `--release --locked --all-targets` (569 testes do executável e 206 do
exemplo), com avisos registrados. O
[CI Linux/macOS de `4614d9e`](https://github.com/guicybercode/kokuban.rs/actions/runs/34618261323)
também passou; o [resumo de validação](linux-evidence/2026-09-11-utf8-dispatch/test-summary.json)
preserva os resultados e hashes dos logs originais.

Os números medem processamento pelo PTY e resposta DSR neste runner.
Não medem apresentação de quadros nem verificam equivalência visual de
Unicode e fontes. Também não estabelecem superioridade sobre outros
terminais ou desempenho em uma máquina Omarchy com GPU e monitor reais.

## Cópia para o histórico: medição pareada em 2026-09-11

A revisão `48afe69` aproveita o sufixo uniforme ao copiar uma linha para o
histórico. Quando esse trecho contém células sem grafema composto, a cópia
preenche texto, cores e atributos diretamente, evitando verificar um `Arc`
ausente em cada célula. Prefixos e sufixos com grafemas compostos continuam
clonando as células, e a linha mantém sua largura completa no histórico.

Cinco pares alternados compararam `4bcf68c` com `48afe69` no mesmo runner
GitHub Ubuntu 24.04 x86_64, AMD EPYC 7763, com ambos os processos na CPU 0.
Cada carga usou aproximadamente 32 MiB, Rust 1.94.1 release, Weston 13 headless
pixman e DejaVu Sans Mono 14. A tela primária manteve histórico de 10.000 linhas
e geometria efetiva de 80×24 células, 720×408 pixels, nas dez amostras.

| Carga com histórico | Anterior, mediana MiB/s | Atual, mediana MiB/s |
| --- | ---: | ---: |
| ASCII | 53,045 | 52,771 |
| ANSI | 42,496 | 47,143 |
| Unicode | 18,380 | 18,336 |
| Linhas curtas | 5,929 | 6,777 |

A razão entre medianas indica **14,3% de ganho em linhas curtas** e **10,9%
em ANSI**. Para ANSI, a mediana das razões calculadas por par é 7,2%; essas
duas formas de agregar as amostras são distintas. ASCII e Unicode variaram
−0,52% e −0,24%, respectivamente, permanecendo próximos dentro da variação
observada. Linhas curtas variaram de 5,628 a 6,057 MiB/s antes e de 6,319 a
6,992 MiB/s depois. O [relatório completo](linux-evidence/2026-09-11-history-copy/ci-primary-report.json)
preserva as amostras, os intervalos e as razões por par.

O [workflow](https://github.com/guicybercode/kokuban.rs/actions/runs/34613367069)
passou. A auditoria conferiu os dez conjuntos de configurações e resultados,
os payloads reproduzidos e os hashes dos executáveis distintos. O
[log de compilação](linux-evidence/2026-09-11-history-copy/ci-primary-build-log.txt)
confirma os dois builds em diretórios separados; seus manifestos e lockfiles
correspondem às revisões Git. Os números medem processamento e resposta DSR
neste cenário, sem medir apresentação de quadros ou comparar outros terminais.

Os testes cobrem estilos completos, duração e identidade dos grafemas
compostos, larguras zero e um, além da comparação diferencial existente com
600 operações. `check`, testes e Clippy passaram em macOS com
`--release --locked --all-targets` (566 testes do executável e 203 do exemplo).
O [CI Linux/macOS de `48afe69`](https://github.com/guicybercode/kokuban.rs/actions/runs/34613309940)
também passou. O [manifesto](linux-evidence/2026-09-11-history-copy/manifest.json)
registra a proveniência e os limites da medição.

## Sufixos de linhas: medição pareada em 2026-09-11

A revisão `4bcf68c` mantém o início do sufixo uniforme de cada linha.
Ao limpar uma linha, ela reescreve somente o prefixo quando o sufixo já é
igual à célula de limpeza, incluindo texto, cores e atributos. A leitura de
metadados também pula o sufixo conhecido como vazio, preservando espaços
impressos explicitamente. O [perfil diagnóstico anterior](linux-evidence/2026-09-11-short-lines/profile-before.txt)
apontou essas operações como custos frequentes durante rolagem de linhas curtas.

Cinco pares alternados compararam `a27b8ce` com `4bcf68c` no mesmo runner
GitHub Ubuntu 24.04 x86_64, AMD EPYC 9V74, com ambos os processos fixados na
CPU 0. Cada carga usou aproximadamente 32 MiB, Rust 1.94.1 release, Weston 13
headless pixman, DejaVu Sans Mono 14 e tela alternativa sem histórico.
A grade efetiva permaneceu em 80×24 células e 720×408 pixels em todas as amostras.

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s |
| --- | ---: | ---: |
| ASCII | 91,64 | 92,26 |
| ANSI | 67,38 | 82,02 |
| Unicode | 19,64 | 27,66 |
| Linhas curtas | 9,62 | 31,58 |

A razão entre medianas foi **3,28× em linhas curtas**, **1,41× em Unicode**
e **1,22× em ANSI**. ASCII variou menos de 1% entre medianas, sem reproduzir
a queda observada na VM local. Os intervalos mínimo–máximo de linhas curtas
foram 9,51–9,63 antes e 31,57–31,71 MiB/s depois. O [relatório completo](linux-evidence/2026-09-11-short-lines/ci-alternate-report.json)
preserva as amostras, as razões por par e as verificações de configuração,
payload e geometria. Esse resultado mede processamento/DSR; não mede
apresentação de quadros nem comprova superioridade sobre outros terminais
no Omarchy com GPU e monitor reais.

O [workflow com builds isolados](https://github.com/guicybercode/kokuban.rs/actions/runs/34610172666)
passou. Os [trechos do log de compilação](linux-evidence/2026-09-11-short-lines/ci-alternate-build-log.txt)
confirmam que os dois diretórios-fonte foram compilados. Os executáveis têm
hashes diferentes; manifesto e lockfile de cada build foram conferidos contra
o respectivo commit Git. Essa verificação exclui o ensaio inválido descrito abaixo.

Outro ensaio usou a tela primária com histórico de 10.000 linhas, mantendo
cinco pares de 32 MiB, as mesmas revisões, fontes e geometria. Esse runner
usou AMD EPYC 7763; os dois executáveis ficaram na CPU 0. Os modelos de CPU
dos dois ensaios diferem, portanto seus valores absolutos não isolam o custo
do histórico. A comparação válida é entre as revisões dentro de cada ensaio.

| Carga com histórico | Anterior, mediana MiB/s | Atual, mediana MiB/s |
| --- | ---: | ---: |
| ASCII | 54,13 | 53,95 |
| ANSI | 39,39 | 45,89 |
| Unicode | 12,75 | 19,05 |
| Linhas curtas | 3,83 | 6,30 |

Neste cenário, a razão entre medianas foi **1,64× em linhas curtas**,
**1,49× em Unicode** e **1,16× em ANSI**. A mediana ASCII caiu 0,3%, dentro
dos intervalos observados de 51,76–55,10 e 51,86–55,40 MiB/s. O
[relatório com histórico](linux-evidence/2026-09-11-short-lines/ci-primary-report.json)
preserva todas as amostras. O [workflow](https://github.com/guicybercode/kokuban.rs/actions/runs/34610695516)
passou, incluindo a nova checagem de executáveis diferentes. Seus hashes
coincidem com os respectivos binários do ensaio de tela alternativa, e o
[log](linux-evidence/2026-09-11-short-lines/ci-primary-build-log.txt) confirma
as duas compilações independentes.

Uma triagem do decoder em macOS, com três pares alternados de 4 MiB, grade
120×40 e histórico de 10.000 linhas, registrou as seguintes medianas:

| Carga | Anterior `a27b8ce`, MiB/s | Atual `4bcf68c`, MiB/s |
| --- | ---: | ---: |
| ASCII | 207,78 | 216,21 |
| ANSI | 145,25 | 161,27 |
| Unicode | 42,26 | 69,20 |
| Linhas curtas | 7,11 | 10,60 |

Os [resultados individuais](linux-evidence/2026-09-11-short-lines/decoder-screening.json)
excluem PTY, atlas e renderer. São uma triagem em host compartilhado, com
poucas amostras, e não uma classificação de terminais.

Uma tentativa posterior de cinco pares completos em Wayland, com 4 MiB,
mostrou variação alta e queda da mediana ASCII de 159,47 para 129,90 MiB/s,
apesar dos ganhos nas outras cargas. O relatório bruto ficou inacessível após
erros de armazenamento da VM; o [resumo transcrito da saída](linux-evidence/2026-09-11-short-lines/preliminary-vm-summary.json)
identifica essa limitação. A repetição com 32 MiB foi interrompida por erro de
I/O durante o segundo par. Ela não produziu um resultado utilizável.
Essas observações motivaram a repetição independente no CI descrita acima.

O [primeiro ensaio no CI](https://github.com/guicybercode/kokuban.rs/actions/runs/34609339282)
também foi descartado: a conferência dos hashes detectou duas cópias do
binário anterior. O diretório Cargo compartilhado reutilizou a compilação
anterior, apesar dos arquivos-fonte distintos. O [JSON original](linux-evidence/2026-09-11-short-lines/ci-invalid-build.json)
preserva esse diagnóstico; seu status `passed` verifica métricas e geometria,
mas não valida a comparação entre revisões. `c03a131` separa os diretórios de
compilação, e `69dd58d` rejeita executáveis idênticos por padrão no comparador.

Os testes diferenciais com 600 operações mistas cobrem o cache de sufixos,
rolagem, metadados, estilos e grafemas compostos; acessos inválidos também
são verificados. `check`, testes e Clippy passaram em macOS (563 testes do
executável e 200 do exemplo) e Linux (661 e 200; um benchmark ignorado).
O clipboard X11 preservou bytes UTF-8 e texto após quatro redimensionamentos.
O [CI Linux/macOS de `4bcf68c`](https://github.com/guicybercode/kokuban.rs/actions/runs/34606995498)
passou. O [manifesto](linux-evidence/2026-09-11-short-lines/manifest.json)
registra hashes, validações e tentativas preliminares, inclusive as inconclusivas.

## Unicode sem alocações intermediárias em 2026-09-11

As revisões `ce4799b` e `c51c006` removem duas alocações frequentes: a decisão
de fronteira de grafema agora consulta `GraphemeCursor` com uma pequena janela
UTF-8 e contexto emprestado; o cache do atlas consulta `&str` em quatro mapas
por estilo. O texto é alocado quando um grafema realmente cresce ou quando
uma chave nova entra no cache. Shaping, cores, fallback e regras de grafemas
permanecem habilitados.

Cinco execuções de cada binário, alternando anterior/atual e atual/anterior,
compararam `b58e499` com `c51c006`. Ambiente: Rust 1.94.1 release, Ubuntu 26.04
arm64 em Docker/Colima sobre Apple M4/macOS, Weston 14 headless com pixman e
renderização por software. Cada carga tinha aproximadamente 4 MiB e terminava
com resposta DSR. Os dois binários usaram a mesma grade efetiva de 80×24,
720×408 pixels, DejaVu Sans Mono 14 e tela alternativa sem histórico.
Não havia outras medições ou compilações desta tarefa durante as amostras.

| Carga | Anterior, mediana MiB/s | Atual, mediana MiB/s |
| --- | ---: | ---: |
| ASCII | 178,65 | 178,18 |
| ANSI | 132,85 | 139,55 |
| Unicode | 24,07 | 31,70 |
| Linhas curtas | 23,07 | 23,69 |

A mediana Unicode aumentou **31,7% neste cenário**; seus intervalos
mínimo–máximo foram 22,12–24,19 e 30,53–32,56 MiB/s. Os intervalos completos
das demais cargas estão no [relatório pareado](linux-evidence/2026-09-11-unicode-performance/paired-wayland.json).
ASCII variou de 168,31 a 239,77 MiB/s antes e de 175,72 a 215,15 depois;
o ambiente compartilhado não permite tratar diferenças pequenas como ganhos
precisos. O ensaio mede processamento/DSR, não apresentação de quadros.

Um ensaio separado do decoder em macOS, com 8 MiB por carga, cinco pares,
120×40 células e 10.000 linhas de histórico, passou de 32,37 para 46,41 MiB/s
medianos em Unicode (**43,4%**). Esse resultado exclui PTY, atlas e renderer;
ele mede a mudança de fronteiras, não o efeito do cache de grafemas.
Os [resultados individuais do decoder](linux-evidence/2026-09-11-unicode-performance/decoder.json)
também preservam as outras cargas, cujas medianas ficaram próximas.

Passaram `check`, testes e Clippy com `--all-targets` em Linux (657 testes do
executável e 196 do exemplo; um benchmark ignorado) e macOS (559 e 196).
Os testes diferenciais verificam fronteiras UAX #29, incluindo ZWJ, indicadores
regionais, Indic, controles e marcas combinantes. O teste real de clipboard X11
também preservou os bytes de grafemas compostos e de uma URL quebrada em linhas
após quatro redimensionamentos sem reimprimir o conteúdo.
O [CI Linux/macOS de `c51c006`](https://github.com/guicybercode/kokuban.rs/actions/runs/34605347739)
também passou.

O [ensaio exploratório dos quatro terminais](linux-evidence/2026-09-11-unicode-performance/exploratory-four-terminals.json)
foi executado antes dessas duas mudanças, com três amostras de 4 MiB por terminal.
Ele ajuda a localizar gargalos, mas registra geometrias diferentes e fidelidade
visual não comparada. Linhas curtas continuam sendo um gargalo. Esses números
não comprovam superioridade geral sobre Ghostty, Alacritty ou Kitty, nem
substituem a comparação no Omarchy/Hyprland com GPU e monitor reais.

O [manifesto](linux-evidence/2026-09-11-unicode-performance/manifest.json) registra
hashes e validações; o [executor do par](linux-evidence/2026-09-11-unicode-performance/paired-pty.py)
reutiliza o comparador do repositório. Em uma sessão Wayland, é possível repetir
o par com os dois binários preservados:

```sh
python3 docs/linux-evidence/2026-09-11-unicode-performance/paired-pty.py \
  scripts/compare-terminal-performance.py /caminho/anterior /caminho/atual \
  /tmp/kokuban-unicode-paired
```

## Otimizações e medição pareada de 2026-09-10

Entre `7e0e9e2` e `da14f90`, o processamento passou a agrupar ASCII imprimível,
rolar índices de linhas em vez de copiar todas as células e desenhar fatias de
pixels já validadas. O renderer Linux agora mantém até quatro snapshots para
identificar o conteúdo dos buffers do softbuffer e repinta a faixa alterada.
Imagens, IME, mudanças de tamanho/fonte e buffers desconhecidos usam desenho
integral. A apresentação continua completa, inclusive para exposição X11.

Na mesma VM Linux arm64 do Colima, sobre macOS/Apple M4, foram executadas
cinco amostras de cada binário em release com Rust 1.94.1, Debian 12 e Xvfb.
A ordem alternou entre anterior/atual e atual/anterior. Cada execução usou o
cenário de 2,5 MiB, 80×24 células, 720×408 pixels e 10.000 linhas de histórico
descrito abaixo, incluindo a resposta DSR final. Não havia outros benchmarks
ou compilações desta tarefa durante as amostras; o host não é uma máquina de
benchmark dedicada.

| Medida | Anterior `7e0e9e2` | Atual `da14f90` |
| --- | ---: | ---: |
| Vazão mediana, MiB/s | 27,68 | 55,39 |
| Intervalo mínimo–máximo, MiB/s | 23,87–37,08 | 42,42–64,55 |
| CPU mediana no intervalo amostrado de saída | 0,10 s | 0,05 s |
| RSS mediano após saída | 27.860 KiB | 28.564 KiB |
| CPU mediana em cinco segundos de repouso após saída | 0,01 s | 0,01 s |
| Executável release | 8.707.392 bytes | 9.301.352 bytes |

A mediana de vazão aumentou **2,00× neste cenário**. O intervalo de CPU é o
intervalo amostrado pelo observador, maior que a escrita/DSR cronometrada pelo
filho. No repouso inicial foram observados 0,01 s antes e 0,02 s depois; a
granularidade dos ticks não permite interpretar essa diferença pequena como
um resultado preciso de consumo ocioso. RSS e tamanho do binário aumentaram;
o intervalo de revisões também inclui a substituição da arte do ícone. Estas
medidas não isolam o efeito de cada commit, nem comprovam menor memória.

O [manifesto do ensaio](linux-evidence/2026-09-10-performance/manifest.json),
os [resultados resumidos](linux-evidence/2026-09-10-performance/summary.json)
e os dez JSONs `01-baseline.json` a `05-candidate.json` no mesmo diretório
preservam todas as amostras, ambiente, hashes e dependências dos binários.
O executável anterior tem SHA-256
`3b1af01d2e2fbfe5def3c52088b10dada46c8c9ec4d0a010709306275573fe9e`;
o atual, `28d8033a4cff1921a58e9d88f1a2561a0333149842911eec330841497dba5e0d`.

Uma medição separada do cálculo de danos e rasterização de 120×40 células,
cinco amostras de 300 alternâncias, passou de 0,3482 para 0,1067 ms medianos
quando somente uma linha muda. Quando toda a tela muda, ficou em
0,3473/0,3520 ms. Cada resultado final foi comparado pixel a pixel com um desenho
integral. O [log](linux-evidence/2026-09-10-performance/frame-repaint.txt) e o
teste ignorado permitem repetir essa medição, que exclui snapshot, PTY e
apresentação:

```sh
cargo test --release --locked --bin kokuban benchmark_frame_repaint -- --ignored --nocapture
cargo run --release --locked --example terminal-throughput -- 16 5
python3 scripts/benchmark-software-raster.py --baseline 7e0e9e2
```

Os testes locais passaram em Linux (605 testes do executável e 187 do exemplo)
e macOS (507 e 187), além do Clippy. O benchmark de frame fica ignorado na suíte
normal. Passaram também imagens Kitty/Sixel, animação sintética, clipboard,
SSH, Neovim, fzf, tmux e lançamento nativo Wayland com Weston. O mpv 0.35.1
da VM não oferece `--vo=kitty`, mas o
[workflow release de `e142905`](https://github.com/guicybercode/kokuban.rs/actions/runs/34533009622)
passou com mpv 0.37: os 72 IDs de quadro foram observados, sem capturas ativas
inválidas, com pausa, retomada e encerramento corretos. O
[CI completo Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34532945439)
também passou. A execução release x86_64 registrou 69,85 MiB/s em AMD EPYC 9V74;
esse runner é diferente do usado na medição de setembro de 2026 abaixo e não
permite atribuir uma razão de ganho entre as duas execuções de CI. Evidências:
[recursos de CI](linux-evidence/2026-09-10-performance/ci-resources.json) e
[vídeo de CI](linux-evidence/2026-09-10-performance/ci-video.json).

O [comparador entre terminais](TERMINAL_COMPARISON.md) executou três amostras de
desenvolvimento em Kokuban, Alacritty 0.11 e Kitty 0.26.5 sob X11 e três amostras
de Kokuban sob Wayland. Ele detectou tamanhos em pixels diferentes e registra
que fidelidade visual não foi verificada, incluindo as limitações de
caracteres combinantes e fallback de fonte existentes na versão medida. A
[implementação posterior de grafemas](TERMINAL_TEXT.md) exige nova medição para
comparar seu desempenho. Esses testes não estabelecem
superioridade sobre Ghostty, Alacritty ou Kitty. Uma validação posterior concluiu
também 24 execuções dos quatro terminais com pacotes do Ubuntu 26.04, incluindo
Ghostty, em X11 e Wayland. Os detalhes e limitações estão na
[validação funcional do comparador](TERMINAL_COMPARISON.md#validação-funcional-dos-quatro-terminais-em-2026-09-10).
Ainda é necessário executar a comparação nas versões usadas no Omarchy,
com GPU/monitor reais e Hyprland.

## Espera ociosa do leitor em `b5952ce`

O leitor de PTY deixou de consultar a cada 25 ms se deveria encerrar. Ele agora
espera indefinidamente por saída/EOF do PTY ou pelo fechamento de escrita de um
socket de cancelamento. O encerramento acorda a espera mesmo quando solicitado
antes de ela começar. A mudança acrescenta dois descritores de socket por leitor
ativo; a escrita de respostas e os lotes de processamento mantêm seu comportamento.

Um rastreamento Linux arm64 com `strace`, Xvfb e a mesma aplicação filha fez uma
consulta DSR, ficou três segundos sem saída e encerrou. Contando apenas a thread
`terminal-reader` ao longo de cada rastreamento completo, incluindo inicialização,
houve **109 retornos por timeout antes e zero depois**. No executável novo, a
espera usa timeout nulo e retorna quando o PTY recebe dados ou EOF. Isso confirma
a remoção do temporizador; uma execução instrumentada não mede economia de CPU
ou bateria, nem os despertares das outras threads.

Os testes da revisão combinada, incluindo as melhorias de preservação de texto,
passaram em Linux (653 do executável e 194 do exemplo; um benchmark ignorado)
e macOS (555 e 194), além de `check` e Clippy com `--all-targets`.
Os casos incluem cancelamento antes/durante a espera, shutdown repetido, drop,
descritores CLOEXEC, EOF, interrupções e ordem das respostas. O executável release
também concluiu o lançamento nativo Wayland e o encerramento do filho.

Evidências: [resumo e hashes](linux-evidence/2026-09-10-idle-reader/summary.json),
[thread anterior](linux-evidence/2026-09-10-idle-reader/before-reader.txt),
[thread atual](linux-evidence/2026-09-10-idle-reader/after-reader.txt) e
[lançamento Wayland](linux-evidence/2026-09-10-idle-reader/wayland-launch.json).

## Resultado de 2026-09-05

O código `29ac690`, publicado na `main`, passou na [medição release](https://github.com/guicybercode/kokuban.rs/actions/runs/34005940902) e no [CI completo Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34005916270), com **582 testes Linux e 484 macOS**. Ambiente: Ubuntu 24.04 x86_64, kernel `6.17.0-1022-azure`, runner com quatro CPUs lógicas AMD EPYC 7763, cerca de 16 GiB de RAM e Xvfb.

| Medida do Kokuban | Resultado |
| --- | --- |
| Executável release, sem strip adicional | 8.363.048 bytes / 7,98 MiB |
| RSS no repouso inicial de cinco segundos | 12.216 KiB / 11,93 MiB |
| CPU nesse repouso inicial | Nenhum tick adicional registrado; não significa consumo zero |
| Saída de 2,5 MiB com resposta final do terminal | 0,126 s / 19,84 MiB/s |
| CPU durante o intervalo amostrado de saída | 0,23 s em 0,152 s de parede / 151,6% de um núcleo |
| RSS após saída, com histórico retido | 28.392 KiB / 27,73 MiB |
| CPU nos cinco segundos após estabilização | 0,01 s / 0,20% de um núcleo |
| Vídeo FFV1 320×180, 12 FPS de origem | 72/72 IDs de quadro observados, pausa/retomada e EOF corretos |
| CPU do terminal durante vídeo | 8,75% de um núcleo, média ponderada pelo tempo ativo |
| RSS máximo amostrado do terminal no vídeo | 29.388 KiB / 28,70 MiB |

O CPU do mpv foi medido separadamente: 4,62% de um núcleo e RSS máximo amostrado de 62.380 KiB. No vídeo, 242 capturas ativas não tiveram quadros inválidos; o limite inferior de transições observadas foi 11,72 Hz. Isso não é uma medição de scanout. A fase de saída pode usar mais de um núcleo porque leitura/decodificação e janela têm threads distintas.

O teste também comparou o ioctl real com a geometria X11: **80×24 células, 720×408 pixels**. O shell pode iniciar antes da conclusão do atlas, por isso a leitura inicial é preservada e a verificação usa outra leitura após a estabilização, antes da escrita cronometrada. Essa checagem não substitui um teste de autodimensionamento do mpv.

Evidência permanente: [amostras de recursos e metadados](linux-evidence/29ac690/resources.json), [amostras de vídeo](linux-evidence/29ac690/video.json), [versão Rust](linux-evidence/29ac690/rustc.txt), [pacotes](linux-evidence/29ac690/system-packages.txt), [dependências Cargo](linux-evidence/29ac690/cargo-tree.txt), [primeiro quadro](linux-evidence/29ac690/video-first.png) e [último quadro](linux-evidence/29ac690/video-final.png). O SHA-256 do executável foi `75b882740774b53beffbf79067e06b96bf63461efa98d8cba4e4f24772b1db19`.

## Cenários e definição das métricas

`scripts/linux-resource-smoke.py` abre uma janela X11 real sob Xvfb, com DejaVu Sans Mono 14, grade 80×24, 10.000 linhas de histórico e gráficos habilitados. Uma aplicação controlada dentro do PTY escreve um prompt e espera a resposta DSR do terminal. Depois de um segundo de estabilização, o observador mede cinco segundos sem saída.

A segunda fase transmite 32.768 linhas ASCII, **2.621.440 bytes (2,5 MiB)**, sem pausas entre as escritas. A geração do conteúdo fica fora do intervalo medido. O processo escreve um marcador final e só publica o resultado depois de receber a posição de cursor esperada do Kokuban. Assim, a vazão inclui escrita e resposta do terminal. A barreira comprova processamento do fluxo; não comprova apresentação de cada linha nem mede FPS.

Após a saída, há um segundo de estabilização e outros cinco segundos sem saída. O histórico cheio pode continuar ocupando memória; não se exige retorno ao RSS inicial. O teste confirma encerramento normal. Em processo separado, `scripts/linux-video-smoke.py --build-profile release` repete o teste de [vídeo com mpv](LINUX_VIDEO.md), verificando pixels e controles.

CPU significa a diferença de `utime + stime` do PID do Kokuban dividida pelo tempo de parede: 100% equivale a um núcleo. Não inclui mpv, shell, Python ou Xvfb. RSS é amostrado em `/proc`; HWM é o pico reportado pelo kernel durante a vida do processo. A identidade inclui o instante de criação do PID. Os JSONs preservam todas as amostras e o intervalo real.

Os cenários são curtos e executados em hardware compartilhado de CI. A granularidade dos ticks limita diferenças pequenas de CPU; RSS também é uma estimativa assíncrona do kernel. Há custo indireto da captura/observação no teste de vídeo. Nenhum limite arbitrário de CPU, memória ou vazão faz o teste passar: os critérios automáticos são conclusão funcional, integridade e prazo limitado. Esses resultados não comprovam consumo em um computador específico, Wayland, fontes/plugins diferentes ou reprodução prolongada.

## Repetir

No GitHub Actions, execute manualmente o workflow para a revisão que deseja medir. Ele publica o artefato `linux-release-resource-measurements`. Para execução local Linux, com as mesmas dependências do workflow:

```sh
CARGO_INCREMENTAL=0 CARGO_PROFILE_RELEASE_DEBUG=0 \
  cargo build --release --locked
xvfb-run -a -s '-screen 0 800x600x24' timeout 60s \
  python3 scripts/linux-resource-smoke.py "$PWD/target/release/kokuban" \
  --artifacts-dir /tmp/kokuban-release-resources
xvfb-run -a -s '-screen 0 800x600x24' timeout 90s \
  python3 scripts/linux-video-smoke.py "$PWD/target/release/kokuban" \
  --build-profile release --artifacts-dir /tmp/kokuban-release-video
```

Use a mesma carga, configuração, revisão e máquina para comparações. As dependências de vídeo pertencem ao player externo; o inventário de runtime do Kokuban deve ser lido separadamente. Referência das métricas do kernel: [documentação de `/proc`](https://www.kernel.org/doc/html/latest/filesystems/proc.html).
