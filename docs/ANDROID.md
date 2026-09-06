# Android nativo

Desenvolvimento em `codex/android-native`, worktree `../kokuban-android`.
Base inicial `origin/main` em `62bce8e`; os componentes compartilhados de imagens
foram integrados por merges normais em `14972d3` e `4ea2588` (base Linux
`ec72e05`, incluindo animação nativa). Nenhuma alteração da worktree
Linux foi descartada. Esta entrega ainda está em validação: APK gerado não
significa Android pronto.

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
- `src/android_runtime.rs`: HOME e diretórios privados (0700), ambiente e cwd
  passados somente ao filho. Shell de fallback Android: `/system/bin/sh`.
- Configuração: `files/config/kokuban/kokuban.toml`; HOME: `files/home`, ambos
  relativos ao armazenamento privado da aplicação, sem depender do cwd da JVM.
- `src/android_glyph_atlas.rs`: rasterização Rust/fontdue, fontes do sistema,
  atlas A8 de 1 MiB e até 8192 entradas. Dois fallbacks são carregados sob demanda.
- `src/android_images.rs`: seleciona os módulos CPU e delega a `SoftwareGraphics`,
  compartilhado com Linux: decodificação, KittyHandler, posicionamento, ordem de
  imagens, snapshots, cache e animação. Usa `TerminalReader::spawn_with_graphics`
  com eventos ordenados; não duplica parser nem implementação dos protocolos.
  Cache Android limitado a 64 MiB e transmissão Kitty a 32 MiB, respeitando
  limites menores configurados. Um quadro pode reter pixels substituídos até
  sua apresentação; esses limites não são limites globais de PSS. Animações
  visíveis usam `WaitUntil`; superfície ausente ou perda de foco suspendem os
  timers. O núcleo também limita frames e metadados de posicionamento.
- `src/android_input.rs`: composição, commit e tradução para bytes do terminal.
  `src/android_ime.rs` e `android/java/.../KokubanActivity.java`: adaptação dos
  callbacks de InputConnection, viewport e clipboard às mensagens Rust.
  NativeActivity/winit 0.30 não expõe esses eventos de composição sozinho.

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

Não redistribuímos fontes do Android. Fontdue ainda não faz shaping de scripts
complexos nem emoji colorido. CJK pode carregar outlines grandes: o custo real
precisa ser medido. A fonte geométrica em `fonts/android-test.ttf` foi criada
para testes e não entra no APK.

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
Android ainda depende do smoke. Consulte [CLI e limitações de formatos de chave](../android/ssh-client/README.md).
Git, Neovim, tmux, fzf e Rust executam no host SSH; não são binários locais do APK.

O cliente usa russh/Tokio, com ring e rotinas C/assembly transitivas. O atlas é
Rust/fontdue; o contrato NativeActivity/InputConnection e as APIs do sistema
exigem FFI. A lógica do terminal e do cliente SSH continua em Rust.

## Evidência e verificações pendentes

Medições/resultados abaixo têm escopo específico; não extrapolam para os
cenários ainda não executados.

| Evidência | Resultado observado |
| --- | --- |
| Isolamento Git | Worktree irmã, branch própria, commits/push normais sem trailer de coautoria. |
| PTY/runtime no macOS | 51 testes PTY e 6 runtime passaram, incluindo Ctrl-C, SIGWINCH, cwd/env privados e persistência de arquivos. |
| Atlas | 7 testes determinísticos passaram (A8, baseline, estilos, cache e limites). |
| Regressão compartilhada | 447 testes do binário macOS passaram após integrar imagens em `14972d3`, Rust 1.94.1. |
| Entrada Rust | 5 testes de IME e 6 do encoder compartilhado passaram em harness; os testes foram integrados ao binário para o CI. |
| Controles | 6 testes de geometria/gestos passaram; seleção, clipboard e acessibilidade implementados, ainda sem validação aprovada no dispositivo. |
| APK atual ARM64 debug opt1 | Build com SSH, dex e os dois ELF de 16 KiB passou. APK local: 5.640.685 bytes; lib terminal: 3.608.736 bytes; lib SSH: 6.099.768 bytes. Esse perfil não é release. |
| APK inicial ARM64 | Build e assinatura executados localmente com SDK35/NDK27.1/Rust1.94.1. |
| APK com ponte IME | Build completo em `0b0e8a4`: launcher KokubanActivity, hasCode=true, classes.dex de 12.656 bytes, lib debug sem símbolos de 7.490.768 bytes; assinaturas v2/v3 e zipalign16 passaram. |
| Release CI ARM64 | Build de `95684d2` passou: APK 2.564.589 bytes (2,45 MiB); lib terminal 2.887.392 bytes; SSH 3.182.048 bytes. SHA256 no [registro](android-evidence/cf4525c/results.json), artefato no run [34003114279](https://github.com/guicybercode/kokuban.rs/actions/runs/34003114279). |
| AVD local `ygo` | Presente, Android35 ARM64. Inicialização terminou com falta de espaço; variante read-only também falhou. O emulador exige pelo menos 5 GB livres. Nenhuma execução local comprovada. |
| Instalação/shell/lifecycle CI x86_64 | Passou em `cf4525c`: comando por PTY, mesma sessão após Home/retomada e quatro rotações reais. Capturas inspecionadas; Android15/API35 no emulador Pixel7 x86_64. [Evidência preservada](android-evidence/cf4525c/results.json). |
| IME real, toque, seleção/clipboard | Necessitam execução com teclado Android real; `adb input text` não prova composição. |
| SSH/ferramentas | Cliente implementado: 9 testes unitários e 2 testes CLI/servidor no host passaram, incluindo trust, senha sem eco, UTF-8, resize e restauração do terminal. Em Android, passaram rejeição de host desconhecido/alterado, chave pública, sessão interativa e resize remoto 25x46→4x104. Neovim encontrou Esc consumido pelo IME; correção publicada, ainda em revalidação. tmux/fzf/Git/build permanecem pendentes. |
| Foto/animação/vídeo | Pipeline compartilhado e coleta por pixels implementados. Produtor FFmpeg passou 3 testes, incluindo foto única; testes de reconhecimento de pixels passaram. Apresentação Android e consumo ainda dependem do smoke. |
| Consumo | PSS, CPU, latência e fluidez ainda sem medições aprovadas em runtime. Tamanho de build não prova baixo consumo. |

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
As primeiras capturas preservadas mostram [shell](android-evidence/cf4525c/shell.png),
[SSH](android-evidence/cf4525c/ssh.png) e a [falha de Esc no Neovim](android-evidence/cf4525c/neovim-escape-failure.png).
O [PR #8](https://github.com/guicybercode/kokuban.rs/pull/8) permanece em rascunho
até os critérios de execução serem comprovados.

Ainda é necessário concluir e demonstrar todos os cenários de
`SECOND_SESSION_PROMPT.md`: IME real e teclados externos, seleção/clipboard,
SSH com verificação de host, editor/multiplexer/seletor/Git/build remoto,
fotos, atualização animada e vídeo, memória/CPU/latência/quadros em release,
suspensão e rotação repetidas. Não considerar este documento uma declaração de
entrega final.

Referências primárias: [winit Android](https://docs.rs/winit/latest/winit/platform/android/),
[android-activity](https://github.com/rust-mobile/android-activity),
[restrições de execução Android10](https://developer.android.com/about/versions/10/behavior-changes-10#execute-permission),
[espaço requerido pelo emulador](https://developer.android.com/studio/run/emulator-troubleshooting#free-disk-space).
