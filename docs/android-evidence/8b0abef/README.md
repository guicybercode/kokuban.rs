# APK Android validado com CJK por glifo

O [workflow Android 34026733947](https://github.com/guicybercode/kokuban.rs/actions/runs/34026733947)
passou integralmente em `8b0abef`, com o fallback CFF2 de `f280262`. A execução
foi no emulador Pixel 7, Android 15/API 35, x86_64; ARM64 foi compilado e
verificado. Os JSON preservam resultados/amostras e acrescentam run, commit,
artifact e referência de proveniência.

## Resultados

- Lifecycle: shell funcional, mesma sessão após Home/retomada, quatro rotações
  e comandos depois delas.
- Gboard 14.2.09: toque real, seletor de acento, Backspace, Enter, ocultação e
  reabertura. `café` chegou como `636166c3a9` (5 bytes); `ㄱ` + `ㅏ` compuseram
  `가`, recebido como `eab080` (3 bytes). Arquivos `.bin` preservam os bytes
  brutos. Houve dois preedits não vazios e dois commits no estágio Hangul,
  com restauração do layout English.
- A captura [intermediária](hangul-preedit.png) mostra `ㄱ` com fundo/sublinhado;
  a [captura após Enter](hangul-committed.png) mostra `가` e o prompt seguinte.
  O primeiro PNG não afirma capturar o segundo preedit já atualizado para `가`.
- Sete cenários de controles passaram: toolbar, teclas especiais, Esc com IME,
  seleção/copy/paste, scroll, mouse SGR e modificadores Ctrl/Shift/Alt. Eventos
  externos foram injetados pelo Android; não houve periférico USB físico.
- SSH debug/release: trust, chave pública, resize estrito `25 46` → `4 104`
  (linhas, colunas), Neovim 0.9.5, tmux 3.4, fzf 0.44.1, Git 2.55.0, Rust
  1.94.1, desconexão e shell local. Credenciais temporárias removidas, sem
  erros de limpeza. O preparo de trust do cenário release usa debug antes da
  atualização assinada, conforme os campos de perfil nos JSONs.
- Mídia debug/release: foto NASA AS17-148-22727 e remoção, Sixel, animação Kitty
  após o emissor encerrar, camadas negativas e vídeo H.264 silencioso via SSH.
  As capturas release coincidiram com frames decodificados 11, 30, 47 e 67.

O [CI desktop](desktop-ci.json) também passou: **622 testes Linux e 524 macOS**,
sem falhas/ignorados, check, Clippy, isolamento e cenários de janela real Linux.
Os runs [34026733983](https://github.com/guicybercode/kokuban.rs/actions/runs/34026733983)
e [34026731745](https://github.com/guicybercode/kokuban.rs/actions/runs/34026731745)
correspondem aos eventos push/PR; suas contagens não são somadas em duplicidade.

## APKs

| Alvo release | APK | Biblioteca terminal | Cliente SSH |
| --- | --- | --- | --- |
| ARM64 | 2.609.645 bytes (2,49 MiB) | 2.973.248 bytes | 3.182.048 bytes |
| x86_64 | 2.765.287 bytes (2,64 MiB) | 2.953.112 bytes | 3.826.096 bytes |

As proveniências registram SHA256, tamanho e origem. O APK ARM64 está no
[artifact do run](https://github.com/guicybercode/kokuban.rs/actions/runs/34026733947/artifacts/9987326910),
SHA256 `9bd0a77816d1e47a22d41a4ed3811690181f0984dc830e01a3ce8a4f3aeee78b`.
O artefato final x86_64 contém debug, release e medições. Os APKs usam
assinatura de teste, conforme as instruções de build; não incluem chaves SSH
nem as fontes geométricas/probe de teste.

## Consumo e tempos

| Cenário | PSS app, início → fim | CPU app / árvore, um núcleo | Quadros/correlação |
| --- | --- | --- | --- |
| Release ocioso, 15 s após espera de 3 s | 26.181 → 30.313 KiB | 0,2% / 0,2% | zero novos quadros |
| Release, dez comandos de eco em 15 s | 30.389 → 30.647 KiB | 16,07% / 16,07% | 78 quadros, 74 correlações |
| Release, vídeo 320×180@12 por SSH, 8 s | 41.338 → 34.077 KiB | 56% / 57,125% | 165 apresentações, 18,43 chamadas/s |
| Debug opt1, primeira composição Hangul | 40.834 → 85.084 KiB | não medida neste cenário | diferença de PSS 43,21 MiB |

No eco, a mediana callback→próxima apresentação com saída PTY foi **40,3785 ms**,
p95 **78,49385 ms**, por `statistics.quantiles(samples, n=100, method="inclusive")[94]`.
A média de desenho/apresentação foi 43,6031 ms. No vídeo, foi 41,8796 ms, com
maior intervalo entre apresentações de 143,325 ms.

PSS/CPU da árvore também estão nos JSONs. A CPU não inclui todo o emulador nem
o host SSH. O repouso ainda inclui estabilização do app; o vídeo inclui SSH,
IME visível e capturas. As medidas CJK são dois snapshots do mesmo processo,
sem Gboard/shell, e não isolam a alocação da fonte. A fonte Noto é retida sob
demanda: seu custo não deve ser extrapolado do cenário ASCII ocioso.

Apresentações não equivalem a scanout, e a taxa de chamadas não é FPS do vídeo
efetivamente exibido. O eco não mede dedo/teclado físico até a tela. As
execuções são curtas e em emulador; não estabelecem desempenho prolongado ou
bateria em telefones físicos. Os dados anteriores permanecem como histórico,
sem atribuir diferenças entre runners a uma única alteração de código.
