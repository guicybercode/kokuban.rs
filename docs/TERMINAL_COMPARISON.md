# Comparação reproduzível entre terminais

`scripts/compare-terminal-performance.py` executa Kokuban, Ghostty, Alacritty e
Kitty na mesma sessão Linux. Cada janela recebe a mesma aplicação Python por
PTY, os mesmos bytes e uma consulta final de posição do cursor (DSR). O resultado
mede processamento de saída e resposta do protocolo; não mede apresentação de
quadros, latência entre teclado e tela, nem estabelece o melhor terminal.

## Executar no Omarchy

Compile Kokuban em release e mantenha os outros terminais instalados. Em uma
sessão Wayland, fora de tmux/SSH, execute:

```sh
cargo build --locked --release
python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" \
  --backend wayland --samples 7 --bytes 16777216 --match-cell-size \
  --environment-note 'Descreva GPU, compositor, monitor e escala desta máquina' \
  --output-dir /tmp/kokuban-comparison-wayland
```

O diretório de saída deve ser novo. Use `--ghostty`, `--alacritty` e `--kitty`
para selecionar executáveis específicos. O padrão solicita todos os quatro:
a calibração exige os executáveis solicitados e suas versões identificáveis.
Falhas ficam registradas e causam retorno diferente de zero, preservando a
evidência já produzida. Para um ensaio explicitamente parcial, use, por exemplo,
`--terminals kokuban alacritty kitty`.

Para desenvolvimento em X11, um ensaio menor é:

```sh
xvfb-run -a -s '-screen 0 1280x900x24 -noreset' \
  python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" \
  --backend x11 --samples 3 --bytes 2097152 \
  --output-dir /tmp/kokuban-comparison-x11
```

Xvfb e renderização OpenGL por software servem para desenvolver o ensaio. Use a
GPU, o compositor e o monitor reais para conclusões sobre o Omarchy. Mantenha as
janelas visíveis, no mesmo monitor e escala, e suspenda outras compilações ou
medições durante o ensaio. Um gerenciador de janelas pode substituir o tamanho
solicitado; o relatório registra o tamanho efetivo antes e depois de cada carga.

## Executar a comparação de processamento no CI

O workflow manual `Linux four-terminal processing comparison` usa a imagem
oficial Ubuntu 26.04 amd64 em um runner GitHub Ubuntu 24.04. Ele resolve e
registra o digest da imagem, instala os três concorrentes dos repositórios
Ubuntu e compila o commit solicitado de Kokuban com Rust 1.94.1:

```sh
gh workflow run linux-terminal-comparison.yml -f revision=main
```

Depois dos testes Rust e do lançamento real de Kokuban em Wayland, o workflow
calibra as células e executa cinco amostras de aproximadamente 32 MiB por carga
e terminal, com ordem rotativa e tela alternativa. Usa a afinidade completa do
runner, Weston headless pixman e Mesa por software. DejaVu Sans Mono, Noto CJK
e Noto Color Emoji estão instaladas; versões dos pacotes, fontes/fallbacks e
hashes dos arquivos ficam no artefato. Os relatórios, configurações, logs e
proveniência são mantidos por sete dias mesmo em falhas. Os payloads `.bin`
não são enviados, mas seus tamanhos e hashes permitem verificar a reprodução.

Esse ambiente permite comparar processamento no mesmo sistema; não reproduz
GPU, monitor, Hyprland ou todas as versões instaladas no Omarchy.

## Revisão `5c1f436`: comparação atualizada em 2026-09-11

O [run `34620294348`](https://github.com/guicybercode/kokuban.rs/actions/runs/34620294348)
mede Kokuban após as alterações de UTF-8, linhas circulares e classificação
de controles ASCII. O fonte é `5c1f436`; o executor é `e46f8f6`.
Foram cinco amostras por terminal, quatro cargas de aproximadamente 32 MiB
por amostra e oito pré-voos separados das estatísticas.

A sessão usou Ubuntu 26.04 amd64 em container, AMD EPYC 7763, afinidade nas
quatro CPUs disponíveis e Weston 14.0.2 headless com Pixman. Todos usaram
tela alternativa sem histórico, DejaVu Sans Mono a 14 pixels lógicos
(10,5 pontos nos concorrentes) e células efetivas de 9×17 pixels. A grade
permaneceu em 80×24 células e 720×408 pixels nas 80 cargas cronometradas.
Imagens estavam desabilitadas no Kokuban; as cargas são de texto e controles.

| Carga | Kokuban, mediana MiB/s | Ghostty | Alacritty | Kitty |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 67,05 | 27,50 | 50,83 | 78,03 |
| ANSI | 55,99 | 21,63 | 53,67 | 16,41 |
| Unicode | 21,60 | 26,35 | 52,27 | 59,89 |
| Linhas curtas | 28,46 | 24,50 | 49,69 | 28,66 |

Neste ensaio, Kokuban ficou acima dos três em ANSI e abaixo dos três em
Unicode. Em ASCII superou Ghostty e Alacritty, permanecendo abaixo de Kitty.
Em linhas curtas ficou acima de Ghostty e abaixo de Alacritty; a mediana
ficou próxima de Kitty, com intervalos sobrepostos: 28,27–28,88 MiB/s no
Kokuban e 24,99–29,78 no Kitty. A meta de superar todos os concorrentes
continua sem comprovação.

O RTT mediano DSR foi de 0,075 ms no Kokuban, 0,094 no Ghostty, 0,093 no
Alacritty e 3,198 no Kitty, com 150 observações por terminal. Esse tempo
mede resposta de protocolo, sem medir apresentação de quadros ou latência
entre teclado e tela. O
[relatório bruto](linux-evidence/2026-09-11-current-terminals/ci-report.json)
preserva todas as amostras e pré-voos. O
[manifesto](linux-evidence/2026-09-11-current-terminals/manifest.json) registra
a conferência dos 28 processos, configurações, payloads reproduzidos,
geometria e estatísticas recalculadas. Passaram 672 testes Linux do
executável e 211 do exemplo, com um benchmark ignorado, além do lançamento
Wayland com argumentos literais, diretório com espaços e resposta DSR.

As versões foram Alacritty `0.16.1`, Kitty `0.45.0` e Ghostty
`1.3.0-dev+0000000`, canal `tip` (pacotes `0.16.1-2ubuntu1`,
`0.45.0-1build1` e `1.3.0~us1-0ubuntu1.1`, respectivamente). Noto CJK e Noto
Color Emoji estavam instaladas. Os hashes de executáveis e fontes foram
registrados no CI; seus bytes não foram retidos para rehash independente.
O workflow solicita Mesa/llvmpipe, sem observar `GL_RENDERER` por terminal.
Geometria igual não confirma renderização equivalente de fontes ou Unicode.
Faltam comparação visual e medições no Omarchy com GPU e monitor reais.

Este run compara os quatro terminais na mesma sessão. A tabela anterior,
preservada abaixo, pertence a outra execução e não constitui um ensaio
pareado antes/depois. Os efeitos isolados das alterações e suas perdas
estão em [medições pareadas Linux](LINUX_PERFORMANCE.md).

## Medição com células iguais em 2026-09-11

O [workflow corrigido](https://github.com/guicybercode/kokuban.rs/actions/runs/34615352199)
concluiu cinco amostras por terminal, quatro cargas de aproximadamente 32 MiB
por amostra e oito pré-voos separados das estatísticas. O código de Kokuban
é `48afe69`; o executor e o workflow são de `4184729`. A sessão usou Ubuntu
26.04 amd64 em container, AMD EPYC 7763 e afinidade nas quatro CPUs disponíveis
do runner, com Weston 14 headless pixman e renderização por software.

Todos usaram tela alternativa sem histórico, DejaVu Sans Mono no tamanho
solicitado de 14 pixels lógicos e grade efetiva de 80×24 células, 720×408 pixels.
O perfil desabilitou imagens no Kokuban; as cargas contêm somente texto e
controles de terminal.
A calibração manteve Kokuban em 9×17 pixels por célula e acrescentou espaçamento:
1 pixel em largura/altura no Ghostty e 1 pixel em largura no Alacritty e no
Kitty. A geometria permaneceu estável nas 80 cargas cronometradas.

As versões foram Alacritty `0.16.1` (pacote `0.16.1-2ubuntu1`), Kitty `0.45.0`
(pacote `0.45.0-1build1`) e Ghostty `1.3.0-dev+0000000`, canal `tip` (pacote
`1.3.0~us1-0ubuntu1.1`). Esse Ghostty não se identifica como release estável.
Kokuban foi compilado em release com Rust 1.94.1 e lockfile.

| Carga | Kokuban, mediana MiB/s | Ghostty | Alacritty | Kitty |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 64,85 | 27,05 | 50,87 | 78,69 |
| ANSI | 54,54 | 22,10 | 53,97 | 16,62 |
| Unicode | 20,54 | 25,96 | 52,39 | 59,85 |
| Linhas curtas | 22,45 | 23,87 | 48,82 | 29,28 |

Neste ensaio, Kokuban processou ASCII acima de Ghostty e Alacritty, mas abaixo
de Kitty. Em ANSI ficou próximo de Alacritty, com intervalos sobrepostos
(53,96–55,83 e 53,66–54,36 MiB/s), e acima dos outros dois. Em Unicode e linhas
curtas ficou abaixo dos três. Esses dois caminhos continuam sendo prioridades
de investigação; os dados não sustentam a meta de superar todos os concorrentes.

O RTT mediano do protocolo, com 150 observações por terminal, foi de 0,069 ms
no Kokuban, 0,107 ms no Ghostty, 0,092 ms no Alacritty e 3,200 ms no Kitty.
Esse tempo mede a resposta DSR e não a latência entre teclado e tela. O
[relatório bruto](linux-evidence/2026-09-11-matched-terminals/ci-report.json)
preserva todas as amostras, intervalos, CPU/RSS observados, configurações,
hashes dos executáveis e pré-voos.

Noto CJK e Noto Color Emoji estavam instaladas, com a cadeia de fallback do
Fontconfig e os hashes dos arquivos registrados. Isso não confirma que cada
terminal desenhou os mesmos glifos. O workflow força software Mesa/llvmpipe,
mas não registra `GL_RENDERER` efetivo por terminal. Com células iguais, a
conclusão permanece restrita a processamento neste ambiente; faltam comparação
visual e medições de apresentação/entrada no Omarchy com GPU e monitor reais.

Passaram 664 testes Linux do executável e 203 do exemplo, com um benchmark
ignorado, além do lançamento Wayland com diretório contendo espaços,
argumentos literais e resposta DSR. O [manifesto](linux-evidence/2026-09-11-matched-terminals/manifest.json)
preserva a proveniência e também registra a tentativa anterior: o
[run `34614491270`](https://github.com/guicybercode/kokuban.rs/actions/runs/34614491270)
concluiu a medição, mas perdeu o upload por permissões de caches criados pelo
container. Seus números não foram recuperados nem usados nesta tabela.

## Cargas e amostragem

Cada amostra abre um processo novo e executa quatro cargas: linhas ASCII, SGR com
cores ANSI/truecolor, texto UTF-8 com caracteres largos e combinantes, e linhas
curtas de um caractere. O tamanho solicitado é arredondado para um número inteiro
de linhas, igualmente para todos os terminais. O executor mantém os arquivos
`.bin` localmente e registra seus hashes; o upload de artefatos do CI exclui
esses arquivos. A geração/leitura ocorre antes da medição.

Há estabilização inicial de um segundo, cinco consultas de aquecimento e 30
medições de RTT do protocolo. Antes de cronometrar cada carga, a mesma carga é
executada uma vez para aquecer o parser e as fontes, e a tela é limpa. A medição
termina quando a aplicação recebe a posição esperada após um marcador final.
As respostas DSR são verificadas, inclusive quando a escrita termina antes do
terminal processar todos os bytes.

O programa alterna a ordem dos terminais entre amostras. O padrão usa cinco
amostras; o mínimo é três. `report.json` preserva os tempos individuais, mediana,
mínimo, máximo e desvio absoluto mediano, além da versão e hash dos executáveis,
comandos usados, conteúdo/hash de configuração, geometria do PTY e logs. Também
registra informações de CPU, afinidade e seleção de fonte pelo Fontconfig. Use
`--environment-note` para registrar GPU, compositor e escala; a existência de
uma sessão Wayland ou X11 não comprova aceleração por GPU. O ensaio também
registra CPU e RSS do processo principal do terminal. CPU tem a granularidade
dos ticks de `/proc`; processos auxiliares, Python e compositor não entram nessa
contagem. Para cargas muito rápidas, aumente `--bytes` e o número de amostras.

## Configuração e comparabilidade

O perfil padrão usa a tela alternativa, sem histórico. Isso evita atribuir a
mesma quantidade de linhas a políticas de retenção diferentes. Para investigar
o caminho da tela principal com histórico, execute também:

```sh
python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" --backend wayland \
  --screen primary --scrollback-lines 10000 --ghostty-scrollback-bytes 67108864 \
  --samples 7 --bytes 16777216 --output-dir /tmp/kokuban-comparison-history
```

Kokuban, Alacritty e Kitty recebem limite em linhas. Ghostty usa um orçamento em
bytes que também inclui a tela ativa; não há equivalência garantida com 10.000
linhas. O relatório marca essa comparação como não equivalente. As opções de
Ghostty, incluindo isolamento de configuração e execução em processo separado,
seguem sua [referência oficial](https://ghostty.org/docs/config/reference).

Todos recebem DejaVu Sans Mono e uma grade solicitada de 80×24. O tamanho padrão
de Kokuban é 14 pixels lógicos; nos outros terminais, o ensaio usa 10,5 pontos,
equivalentes a 14 pixels a 96 DPI. Escala, arredondamento e métricas podem diferir.
O relatório aponta diferenças nos pixels ou células reais e mudanças de
geometria durante a carga; nessas condições, não se deve ordenar os resultados
como comparação equivalente. `--columns`, `--rows` e `--font-pixels` permitem
repetir o cenário com outro tamanho solicitado.

Com `--match-cell-size`, dois pré-voos ficam fora das estatísticas. O primeiro
mede a largura e a altura reais de cada célula; o segundo confirma os ajustes
de espaçamento para atingir as maiores dimensões observadas. A fonte e seu
tamanho solicitado permanecem os mesmos. Os ajustes usam `adjust-cell-width`
e `adjust-cell-height` no Ghostty, `font.offset` no Alacritty e `modify_font` no
Kitty. Versões desconhecidas ou anteriores aos pisos verificados são rejeitadas:
Ghostty 1.0.0, Alacritty 0.10.0 e Kitty 0.26.0. Esses pisos não representam
necessariamente a primeira versão que ofereceu cada opção.

O campo `cell_size_calibration` guarda as configurações, respostas, alvo em
pixels e deltas aplicados. A calibração exige a grade solicitada, células com
dimensões inteiras e geometria estável. Se outro terminal exigir uma célula maior
que a de Kokuban, o ensaio falha porque Kokuban ainda não oferece ajuste de
espaçamento. Em um gerenciador que substitui o tamanho solicitado, prepare as
janelas para respeitar a grade antes do ensaio. Igualar as células não verifica
o desenho dos glifos nem a fidelidade visual.

As configurações são isoladas das preferências do usuário. Alacritty anterior
a 0.13 recebe YAML e versões posteriores recebem TOML, com invocação pela
[CLI documentada](https://alacritty.org/cmd-alacritty.html). Kitty recebe
`--config NONE` e opções explícitas de fonte, janela e histórico, conforme a
[CLI](https://sw.kovidgoyal.net/kitty/invocation/) e a
[configuração oficial](https://sw.kovidgoyal.net/kitty/conf/).

O campo `comparability` informa amostras ausentes, geometria ou retenção
incompatível. Qualquer reprovação nesse campo retorna erro, inclusive sem
calibração. Mesmo quando essas verificações passam, a conclusão se limita à
vazão e ao RTT medidos naquele ambiente. O relatório mantém
`rendering_equivalence_verified: false`: a consulta DSR confirma a resposta após
o marcador, mas não verifica o conteúdo do texto nem os pixels apresentados.
A versão anterior à [preservação de grafemas](TERMINAL_TEXT.md) sobrescrevia
marcas combinantes e não buscava fontes alternativas. A implementação atual
preserva essas sequências e usa fallback, mas este benchmark continua sem
comparar pixels entre terminais. Seus tempos não comprovam desempenho superior
com a mesma fidelidade de texto; medições anteriores permanecem associadas aos
commits registrados.

O programa não gera ranking. Os pacotes de uma distribuição de testes não
representam necessariamente as versões usadas no Omarchy; a comparação completa
exige repetir as medições nessa instalação, com GPU e monitor reais.

## Validação funcional dos quatro terminais em 2026-09-10

O comparador concluiu 24 execuções: três amostras de cada terminal em X11/Xvfb
e três em Wayland nativo com Weston 14.0.2, sem `DISPLAY`. Cada execução
processou as quatro cargas de aproximadamente 64 KiB, validou as respostas DSR
e encerrou normalmente. Os concorrentes vieram dos repositórios do Ubuntu 26.04
arm64; Kokuban foi o executável release da revisão `da14f90`.

| Terminal | Versão do pacote/binário | Grade efetiva | Pixels do PTY |
| --- | --- | --- | --- |
| Kokuban | `da14f90`, Rust 1.94.1 | 80×24 | 720×408 |
| Ghostty | pacote `1.3.0~us1-0ubuntu1.1`; binário `1.3.0-dev+0000000` | 80×24 | 640×384 |
| Alacritty | pacote `0.16.1-2ubuntu1` | 80×24 | 640×408 |
| Kitty | pacote `0.45.0-1build1` | 80×24 | 640×408 |

O ambiente foi Docker em uma VM Colima sobre macOS/Apple M4, com renderização
por software e outros testes/compilações em andamento. Os tempos foram
preservados para auditoria do comparador, **não como evidência de desempenho**.
Ambos os relatórios detectaram a diferença de geometria e mantiveram
`ranking: null` e `rendering_equivalence_verified: false`. Esses relatórios
preservam as limitações Unicode da revisão `da14f90`, anterior ao suporte a
grafemas e fallback descrito acima.

Evidências: [relatório X11](linux-evidence/2026-09-10-modern-terminals/x11.json),
[relatório Wayland](linux-evidence/2026-09-10-modern-terminals/wayland.json),
[pacotes](linux-evidence/2026-09-10-modern-terminals/system-packages.txt) e
[manifesto](linux-evidence/2026-09-10-modern-terminals/manifest.json).
