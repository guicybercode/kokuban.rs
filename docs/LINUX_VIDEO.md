# Vídeo Linux com mpv

O Kokuban recebe os quadros do mpv pelo protocolo Kitty no PTY e apresenta as imagens com o renderer Rust por CPU. O mpv externo decodifica o arquivo; o terminal não inclui um decoder de vídeo. A transferência direta dispensa arquivos compartilhados e pode atravessar SSH, mas o cenário abaixo foi executado localmente em X11.

## Resultado executado

O código `c02228d`, integrado à `main`, passou no [CI Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34005117373): **582 testes Linux e 484 macOS**, check, Clippy, isolamento de dependências, gráficos/animação, clipboard, SSH e aplicações. O novo teste `scripts/linux-video-smoke.py` executou **mpv 0.37.0 e FFmpeg 6.1.1** com um vídeo FFV1/Matroska gerado pelo ffmpeg: **320×180, 72 quadros, 12 FPS de origem e seis segundos**.

As capturas identificaram todos os IDs de quadro de 0 a 71 em ordem. Foram 243 capturas durante reprodução e nenhuma inválida. A pausa por Space manteve o quadro 19 estável durante um segundo; outro Space retomou a reprodução. O quadro final permaneceu estável, o player chegou ao EOF e ambos os processos saíram com status zero. As teclas passaram pela janela e pelo PTY; o IPC do mpv apenas observou propriedades e pediu o encerramento final.

Os [dados completos](linux-evidence/c02228d/report.json) e as capturas do [primeiro quadro](linux-evidence/c02228d/frame-00.png), [pausa](linux-evidence/c02228d/paused.png) e [último quadro](linux-evidence/c02228d/frame-71.png) estão preservados no repositório.

| Medida no runner Xvfb, perfil debug | Resultado |
| --- | --- |
| Transições observadas / tempo ativo | 11,50 Hz, limite inferior de amostragem |
| Intervalo mediano entre capturas | 25,07 ms |
| CPU Kokuban antes/depois da pausa | 40,6% / 42,6% de um núcleo |
| RSS máximo amostrado Kokuban | 38.484 KiB |
| RSS máximo amostrado mpv, separado | 62.828 KiB |

Esses números incluem o custo indireto da observação e **não são um benchmark release**, nem medem scanout ou garantem FPS sustentado para outras resoluções. O teste usa um clipe curto e sem áudio; codecs diferentes, vídeo por SSH, sincronização audiovisual, Wayland e reprodução prolongada exigem validação própria. As dimensões do mpv foram fixadas; o teste não comprova autodetecção de tamanho nem resize/DPI durante vídeo.

Uma [medição posterior em release](LINUX_PERFORMANCE.md), em `29ac690`, repetiu os 72 quadros e controles com 8,75% de um núcleo para o Kokuban e pico amostrado de 28,70 MiB de RSS. Os cenários e as limitações permanecem os mesmos; o relatório separa consumo de terminal e player.

## Experimentar

Dentro do Kokuban Linux, com mpv instalado:

```sh
mpv --no-config --load-scripts=no --vo=kitty --vo-kitty-use-shm=no \
  --profile=sw-fast --audio=no --osc=no --osd-level=0 \
  --vo-kitty-width=320 --vo-kitty-height=180 \
  --vo-kitty-cols=80 --vo-kitty-rows=24 \
  --vo-kitty-left=1 --vo-kitty-top=1 video.mkv
```

Space pausa/retoma e `q` encerra o mpv. O exemplo fixa os mesmos parâmetros gráficos do teste. Para repetir a verificação automática, instale as dependências do workflow e execute:

```sh
cargo build --locked
xvfb-run -a -s '-screen 0 800x600x24' timeout 90s \
  python3 scripts/linux-video-smoke.py "$PWD/target/debug/kokuban" \
  --artifacts-dir /tmp/kokuban-video-evidence --build-profile debug
```

## Desenho e memória

Versões verificadas do mpv criam novas imagens anônimas para cada quadro. O snapshot deixa de desenhar uma imagem totalmente coberta por outra imagem opaca posterior, respeitando camadas, transparência e cobertura dos pixels. A busca considera até 64 retângulos por imagem. As imagens continuam no cache e os placements continuam válidos: apagar ou tornar transparente o quadro superior pode revelar o anterior.

O limite de cache permanece configurável em `images.cache.max_memory_mb`, com padrão de 256 MiB e limite adicional de imagens/placements. A otimização reduz desenho redundante, não elimina a retenção de todos os quadros anteriores antes da evicção. Snapshots podem manter buffers vivos temporariamente além do limite do cache. O teste de 100 quadros anônimos verifica um único desenho por snapshot, com os 100 dados ainda retidos e revelação após exclusão.

O Linux agora informa tamanho físico da área desenhável no `TIOCSWINSZ`, inclusive mudanças de pixels que preservam a quantidade de células. A chamada aditiva `Pty::resize_with_pixels` preserva a API anterior e mantém o grid inalterado se o ioctl falhar. O teste de PTY real lê os quatro campos de `TIOCGWINSZ`.

Fontes do contrato externo: [saída Kitty do mpv 0.37.0](https://github.com/mpv-player/mpv/blob/v0.37.0/video/out/vo_kitty.c), [opções de vídeo do mpv 0.41.0](https://github.com/mpv-player/mpv/blob/v0.41.0/DOCS/man/vo.rst) e [consulta do tamanho de terminal](https://github.com/mpv-player/mpv/blob/v0.41.0/osdep/terminal-unix.c).
