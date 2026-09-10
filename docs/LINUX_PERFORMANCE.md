# Medições Linux em release

O workflow manual `Linux release resource measurements` compila com Rust 1.94.1 e `cargo build --release --locked`, sem strip ou LTO adicionais. Ele registra manifesto, lockfile, árvore Cargo, versões de pacotes, dependências ELF, tamanho/hash do binário e hardware do runner. O perfil de release mantém `debug=0`.

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
