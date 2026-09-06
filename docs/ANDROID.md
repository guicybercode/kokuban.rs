# Android nativo

Desenvolvimento em `codex/android-native`, worktree `../kokuban-android`.
Base inicial `origin/main` em `62bce8e`; os componentes compartilhados de imagens
foram integrados por merges normais em `14972d3` e `4ea2588`; `3775e6a`
incorporou a base Linux `385a91c`, com seleção/clipboard, dimensões físicas
do PTY e descarte de desenhos de imagens opacas que cobrem outras imagens.
O merge `5638fab` incorporou a documentação Linux de `46e74fb`. Nenhuma alteração da worktree
Linux foi descartada. A matriz Android de `8b0abef` passou integralmente no
emulador Android 15/API 35 x86_64, incluindo IME real, controles, SSH, mídia e
medições release. O build ARM64 também passou. [APK, evidências e limites](android-evidence/8b0abef/README.md).

## Build e instalação

Pré-requisitos: Rust 1.94.1, Java 17, SDK Android 35/build-tools 35.0.0,
NDK 27.1.12297006 e cargo-apk 0.10.0. O wrapper fixa tanto Cargo quanto rustc
para evitar a mistura de artefatos da instalação Homebrew e rustup.

```sh
rustup toolchain install 1.94.1 --profile minimal --component clippy
rustup target add --toolchain 1.94.1 aarch64-linux-android
rustup run 1.94.1 cargo install cargo-apk --version 0.10.0 --locked
export ANDROID_HOME="$HOME/Library/Android/sdk"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
# O wrapper usa opt-level=1 para debug Android; medições finais usam release.
scripts/android/build.sh debug aarch64-linux-android
adb install -r target/debug/apk/kokuban.apk
adb shell am start -n com.kokuban.terminal/.KokubanActivity
```

Para release de teste: `scripts/android/build.sh release aarch64-linux-android
--test-signing` (em uma linha). O resultado fica em
`target/release/apk/kokuban.apk`. A chave de teste é explicitamente inadequada
para distribuição. Para release próprio, configure
`CARGO_APK_RELEASE_KEYSTORE` e `CARGO_APK_RELEASE_KEYSTORE_PASSWORD` no ambiente.
Não adicione chaves ao Git.

`cargo-apk` 0.10 não aceita `--locked` em `build`; o wrapper primeiro executa
`cargo fetch --locked`, compila offline e confere o hash do lockfile. Depois,
compila o adaptador Java com Java 8 bytecode, gera dex com d8 e empacota o
cliente SSH Rust como `libkokuban_ssh.so`, extraído pelo instalador Android.
As flags do linker alinham os segmentos ELF a 16 KiB; o empacotador confere
os segmentos de cada biblioteca, além de zipalign, Activity, `hasCode`, dex e
assinatura. Alinhamento do ZIP sozinho não comprova alinhamento ELF.

Use `--test-signing` tanto no debug quanto no release para atualizar o APK
entre perfis preservando os dados de teste com a mesma chave local.

## Arquitetura e contratos

- `src/lib.rs`: `android_main(AndroidApp)` usando o tipo reexportado por winit.
- `src/android_window.rs`: NativeActivity, softbuffer e sessão PTY. Suspensão
  libera Surface/Context; retomada recria recursos e preserva shell/grid. O
  loop espera eventos, sem redesenho contínuo em repouso.
- `src/android_pty_size.rs`: informa ao PTY a área física do terminal sem barra
  de controles ou IME. Aplica mudanças somente em pixels mesmo quando a grade
  não muda e guarda apenas dimensões aceitas pelo ioctl (`3de3bb3`).
- `src/android_runtime.rs`: HOME e diretórios privados (0700), ambiente e cwd
  passados somente ao filho. Shell de fallback Android: `/system/bin/sh`.
- Configuração: `files/config/kokuban/kokuban.toml`; HOME: `files/home`, ambos
  relativos ao armazenamento privado da aplicação, sem depender do cwd da JVM.
- `src/android_glyph_atlas.rs`: fonte primária rasterizada por fontdue, fontes
  do sistema, atlas A8 de 1 MiB e até 8192 entradas. `android_font_fallback.rs`
  usa ttf-parser e ab_glyph_rasterizer para desenhar somente o glifo solicitado.
  Dois arquivos de fallback de até 32 MiB cada podem ficar retidos; a evicção
  precede a leitura da terceira fonte. Outlines CJK não são materializados em
  conjunto (`f280262`). Os três componentes são Rust.
- `src/android_images.rs`: seleciona os módulos CPU e delega a `SoftwareGraphics`,
  compartilhado com Linux: decodificação, KittyHandler, posicionamento, ordem de
  imagens, snapshots, cache e animação. Usa `TerminalReader::spawn_with_graphics`
  com eventos ordenados; não duplica parser nem implementação dos protocolos.
  Cache Android limitado a 64 MiB e transmissão Kitty a 32 MiB, respeitando
  limites menores configurados. Um quadro pode reter pixels substituídos até
  sua apresentação; esses limites não são limites globais de PSS. Animações
  visíveis usam `WaitUntil`; superfície ausente ou perda de foco suspendem os
  timers. O núcleo também limita frames e metadados de posicionamento.
  Imagens opacas podem evitar o desenho de imagens totalmente encobertas;
  essa otimização não elimina seus dados nem, por si só, seus timers.
  Camadas Kitty abaixo de `i32::MIN / 2` ficam atrás dos fundos explícitos das
  células, mantendo visibilidade através do fundo padrão (`1df1a00`).
- `src/android_input.rs`: composição, commit e tradução para bytes do terminal.
  `src/android_ime.rs` e `android/java/.../KokubanActivity.java`: adaptação dos
  callbacks de InputConnection, viewport e clipboard às mensagens Rust.
  NativeActivity/winit 0.30 não expõe esses eventos de composição sozinho.
- O winit usado somente no Android vem de `vendor/winit-0.30.13`, com origem,
  licença e alterações registradas no [manifesto de manutenção](../vendor/README.md).
  A adaptação entrega mouse nativo e modificadores antes das teclas, preservando
  teclas nomeadas e evitando botões duplicados. Desktop mantém o pacote do registry.
- Colagem usa o encoder compartilhado, normaliza quebras de linha e remove
  controles embutidos. O limite de 64 KiB inclui os marcadores de bracketed paste.
  Até oito leituras assíncronas recebem IDs distintos; respostas obsoletas após
  mudança de tela, modo ou foco são descartadas. Recusa da fila mostra erro e
  preserva a sessão (`7f9075e`). Seleção acompanha o descarte do histórico e
  limita a cópia antes da alocação (`231509c`).

O código do terminal, PTY, parser, renderização e semântica de entrada permanece
Rust. Existe uma pequena ponte Java para os contratos do sistema Android.
JNI, libc/PTY, NativeActivity e softbuffer usam APIs/FFI do sistema. O alvo
Android não usa font-kit, FreeType ou fontconfig. Isso não equivale a afirmar
que todas as dependências e o sistema operacional são escritos em Rust.

## Entrada e fontes

A barra oferece Esc, Tab, Ctrl de um uso, setas, teclado, seleção, copiar e
colar. A página More inclui Home, End, PgUp, PgDn, Insert e Delete. Alvos de
toque têm pelo menos 48dp, com paginação em telas estreitas. Tocar no terminal
alterna o teclado; arrastar rola histórico ou estende a seleção quando o modo
Sel está ativo. Mouse externo e rolagem respeitam o modo solicitado pela
aplicação de terminal. Teclas especiais compartilham o encoder do desktop.

A composição permanece local até commit e recebe indicação visual. O viewport
acompanha teclado, rotação e barras do sistema. A ponte usa um editor de texto
transacional: permite sugestões/composição sem solicitar autocorreção, e pede
que o IME não aprenda os textos. A decisão final de privacidade depende do IME.
Os botões têm nós de acessibilidade Android com rótulo, estado e bounds; isso
não demonstra navegação completa do conteúdo do terminal com TalkBack.

Não redistribuímos fontes do Android. A rota atual ainda não faz shaping de
scripts complexos nem emoji colorido. O fallback CJK é rasterizado por glifo:
o teste isolado de uma fonte Noto variável de 32.355.424 bytes registrou pico
RSS de 34.062.336 bytes, contra 351.305.728 bytes ao expandir todos os outlines
CFF2. Essa medição no host não representa PSS do app. No emulador, a primeira
composição Hangul em debug opt1 registrou PSS de 40.834→85.084 KiB; são dois
snapshots do processo, sem isolar a alocação da fonte. As fontes geométricas de [teste](../fonts/README.md) são originais
e não entram no APK.

O shell de sistema oferece comandos Android/toybox; não constitui um ambiente
completo de desenvolvimento. O cliente SSH Rust é empacotado no APK e chamado
pela função `ssh()` do shell. O arquivo gerado `config/android-shell.rc` carrega
opcionalmente `$HOME/.kokubanrc`; configuração do usuário permanece separada.
Android 10+ restringe execução de binários graváveis no HOME. O executável SSH
fica no diretório nativo extraído pelo instalador, fora desse caminho gravável.

O cliente valida `known_hosts`, rejeita mudanças de chave, pede confirmação
explícita do fingerprint para um novo host e suporta chave pública, senha e
keyboard-interactive. O modo `--batch` rejeita hosts desconhecidos. PTY remoto,
SIGWINCH, saída e restauração do terminal estão implementados; o comportamento
foi verificado em Android para chave pública, trust e sessão interativa
no cenário abaixo. Senha e keyboard-interactive têm validação de host,
mas ainda não foram exercidos pelo teclado virtual Android. Consulte [CLI e limitações de formatos de chave](../android/ssh-client/README.md).
Git, Neovim, tmux, fzf e Rust executam no host SSH; não são binários locais do APK.

O cliente usa russh/Tokio, com ring e rotinas C/assembly transitivas. O atlas é
Rust, usando fontdue e rasterização de fallback por glifo; o contrato NativeActivity/InputConnection e as APIs do sistema
exigem FFI. A lógica do terminal e do cliente SSH continua em Rust.

## Evidência da versão validada

O [run Android 34026733947](https://github.com/guicybercode/kokuban.rs/actions/runs/34026733947)
passou com o código `8b0abef`. Proveniências, SHA256, resultados, bytes brutos,
capturas e amostras estão em [android-evidence/8b0abef](android-evidence/8b0abef/README.md).
Os resultados identificam o perfil e a ABI; não extrapolam para todo dispositivo.

| Evidência | Resultado observado |
| --- | --- |
| Integração e regressão | Worktree própria, merges e pushes normais sem coautoria do Codex. 622 testes Linux e 524 macOS passaram, além de check, Clippy, isolamento e cenários Linux de janela, gráficos, clipboard, mpv e SSH. [CI](android-evidence/8b0abef/desktop-ci.json). |
| Atlas e PTY | 11 testes de fontes cobrem A8, baseline/descender, TrueType/CFF2, Hangul, espaços, curvas e limites. O CI inclui os testes de PTY, runtime, seleção, paste e entrada; 52 testes PTY reais já tinham passado no harness após a integração compartilhada. |
| APK release ARM64 | 2.609.645 bytes (2,49 MiB); biblioteca terminal 2.973.248 bytes, SSH 3.182.048 bytes. Assinatura, dex e alinhamento ELF de 16 KiB verificados. [Download e SHA256](android-evidence/8b0abef/README.md#apks). Execução do dispositivo usa x86_64. |
| Instalação/shell/lifecycle | Sete verificações passaram: comando por PTY, mesma sessão após Home/retomada, quatro rotações e comando posterior. [Resultados](android-evidence/8b0abef/lifecycle.json). |
| IME real | Gboard 14.2.09 passou toque, acento, Backspace, Enter, ocultação/reabertura e composição Hangul. Bytes exatos: `café` = `636166c3a9`, `가` = `eab080`; dois preedits não vazios, dois commits no estágio Hangul, English restaurado. [Resultados](android-evidence/8b0abef/ime.json), [preedit ㄱ](android-evidence/8b0abef/hangul-preedit.png), [가 após Enter](android-evidence/8b0abef/hangul-committed.png). |
| Controles | Sete cenários passaram com bytes exatos: toolbar, teclas especiais, Esc com IME, seleção/copy/paste, scroll, mouse SGR e Ctrl/Shift/Alt. [Resultados](android-evidence/8b0abef/controls.json), [seleção](android-evidence/8b0abef/selection.png), [clipboard](android-evidence/8b0abef/clipboard.png). Entrada externa injetada pelo Android; sem periférico USB físico. |
| SSH e ferramentas | Debug e release passaram trust, chave pública, resize estrito 25x46→4x104, Neovim 0.9.5, tmux 3.4, fzf 0.44.1, Git 2.55.0, build/run Rust 1.94.1 e retorno ao shell local. [Resultados e perfis](android-evidence/8b0abef/release-ssh.json). O cliente também tem 9 testes unitários e 2 testes CLI/servidor no host. |
| Foto/animação/vídeo | Seis cenários passaram em debug e release: foto NASA e remoção, Sixel, animação nativa após o emissor encerrar, camadas Kitty negativas e H.264 silencioso por SSH com frames decodificados distintos. [Resultados](android-evidence/8b0abef/release-media.json), [foto](android-evidence/8b0abef/photo.png), [vídeo](android-evidence/8b0abef/video.png). |
| Repouso release x86_64 | APK 2.765.287 bytes. Em 15 s após 3 s de espera: PSS app 26.181→30.313 KiB; app+shell 27.117→31.249 KiB; CPU 0,2% de um núcleo, zero novos quadros. [Amostras](android-evidence/8b0abef/release-idle.json). |
| Entrada release x86_64 | Dez comandos de eco em 15 s: 78 quadros, 74 correlações callback→próximo quadro de saída, mediana 40,38 ms, p95 78,49 ms; desenho/apresentação médio 43,60 ms. [Amostras](android-evidence/8b0abef/release-echo.json). Não mede teclado físico até scanout. |
| Vídeo release x86_64 | Amostra de 8 s, 320×180@12, SSH e IME visível: PSS app 41.338→34.077 KiB; CPU app 56%, árvore 57,125% de um núcleo; desenho/apresentação médio 41,88 ms, 18,43 chamadas/s. [Amostras](android-evidence/8b0abef/video-measurements.json). Chamadas de apresentação não equivalem a FPS exibido. |
| Primeira carga CJK | Debug opt1, mesmo processo: PSS 40.834→85.084 KiB (39,88→83,09 MiB). O custo é sob demanda; não está representado pelo repouso ASCII. [Snapshots e escopo](android-evidence/8b0abef/ime.json). |
| AVD local | `ygo`, Android35 ARM64, falhou por falta de espaço, inclusive read-only. A execução comprovada veio do CI x86_64; não houve validação local ou em telefone ARM64. |

Ferramentas reproduzíveis:

```sh
python3 scripts/android/smoke.py --apk target/debug/apk/kokuban.apk --serial emulator-5554
python3 scripts/android/measure.py --scenario debug-idle --apk target/debug/apk/kokuban.apk --serial emulator-5554
```

O smoke registra saída por arquivo criado pelo shell dentro do sandbox, retoma
o mesmo processo/shell e verifica rotação. O coletor registra PSS e CPU do
processo principal e da árvore de subprocessos. Gfxinfo bruto não prova FPS do
softbuffer. Criar `files/config/kokuban/trace-frames` antes do lançamento ativa
logs sem texto de entrada: duração de desenho/apresentação e tempo entre
callback de entrada e próximo quadro com saída PTY. A correlação exige cenário
controlado de eco; saída não relacionada pode contaminá-la. Chamadas de
apresentação não equivalem a scanout físico. Evidências
locais vão para `target/android-evidence`, sem credenciais ou dados privados.
Leia também `scripts/android/README.md` e a [rota de mídia](../tools/README.md).
As capturas atuais mostram [Neovim](android-evidence/8b0abef/neovim.png),
[duas panes tmux](android-evidence/8b0abef/tmux.png) e os glifos CJK corrigidos.
O [PR #8](https://github.com/guicybercode/kokuban.rs/pull/8) reúne a implementação
para revisão e integração na `main`.

As medições são curtas e em emulador. O repouso inclui estabilização após
abrir o app; vídeo inclui SSH e screenshots. Não atribuímos diferenças entre
runners a uma única alteração. O [relatório](android-evidence/8b0abef/README.md)
registra dados, fórmula do percentil e escopo. Resultados anteriores ficam
preservados como histórico, incluindo o resize debug descartado em `b158779`
e a correção de glifos CFF2 em `f280262`.

Permanecem limitações de produto/ambiente: fontes sem shaping complexo ou
emoji colorido, vídeo silencioso, ferramentas de desenvolvimento no host SSH,
acessibilidade limitada aos controles, ausência de teste com periférico USB
físico e de medições prolongadas/bateria em telefone real. A validação de
IME é específica ao Gboard testado, não uma certificação de todos os teclados.

Referências primárias: [winit Android](https://docs.rs/winit/latest/winit/platform/android/),
[android-activity](https://github.com/rust-mobile/android-activity),
[restrições de execução Android10](https://developer.android.com/about/versions/10/behavior-changes-10#execute-permission),
[espaço requerido pelo emulador](https://developer.android.com/studio/run/emulator-troubleshooting#free-disk-space).
