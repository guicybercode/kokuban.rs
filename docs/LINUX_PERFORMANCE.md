# Medições Linux em release

O workflow manual `Linux release resource measurements` compila com Rust 1.94.1 e `cargo build --release --locked`, sem strip ou LTO adicionais. Ele registra manifesto, lockfile, árvore Cargo, versões de pacotes, dependências ELF, tamanho/hash do binário e hardware do runner. O perfil de release mantém `debug=0`.

Para comparar duas revisões no mesmo runner Linux, use o workflow manual
`Linux paired revision measurements`:

```sh
gh workflow run linux-paired-performance.yml \
  -f before=a27b8ce -f after=4bcf68c -f screen=alternate
```

Ele resolve os commits, compila os dois arquivos-fonte Git com Rust 1.94.1 e
lockfile, preserva os binários e executa cinco pares alternados com cargas de
32 MiB na mesma CPU permitida. A sessão usa Ubuntu 24.04, Weston headless
pixman, DejaVu Sans Mono 14 e grade 80×24. `screen=primary` habilita histórico
de 10.000 linhas; `alternate` usa a tela alternativa sem histórico. O artefato
retido por sete dias contém JSONs, configurações, logs, hashes e proveniência,
inclusive em caso de falha. Os payloads binários são omitidos do upload;
seus tamanhos e hashes permitem verificar a reprodução pelo gerador do script.

Em uma sessão Wayland Linux existente, o mesmo executor aceita dois binários
previamente compilados. Os rótulos de revisão são declarados pelo operador:

```sh
python3 scripts/compare-kokuban-revisions.py \
  --before /caminho/anterior --after /caminho/atual \
  --before-ref REVISAO_ANTERIOR --after-ref REVISAO_ATUAL \
  --artifacts-dir /tmp/kokuban-paired --backend wayland
```

## Sufixos de linhas e investigação de 2026-09-11

A revisão `4bcf68c` mantém o início do sufixo uniforme de cada linha.
Ao limpar uma linha, ela reescreve somente o prefixo quando o sufixo já é
igual à célula de limpeza, incluindo texto, cores e atributos. A leitura de
metadados também pula o sufixo conhecido como vazio, preservando espaços
impressos explicitamente. O [perfil diagnóstico anterior](linux-evidence/2026-09-11-short-lines/profile-before.txt)
apontou essas operações como custos frequentes durante rolagem de linhas curtas.

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
Essas observações exigem uma repetição independente antes de concluir sobre
a vazão do terminal completo.

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
