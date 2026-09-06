# Medições Linux em release

O workflow manual `Linux release resource measurements` compila com Rust 1.94.1 e `cargo build --release --locked`, sem strip ou LTO adicionais. Ele registra manifesto, lockfile, árvore Cargo, versões de pacotes, dependências ELF, tamanho/hash do binário e hardware do runner. O perfil de release mantém `debug=0`.

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
