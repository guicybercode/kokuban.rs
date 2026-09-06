# Android nativo

Desenvolvimento em `codex/android-native`, worktree `../kokuban-android`.
Base inicial `origin/main` em `62bce8e`; os componentes compartilhados de imagens
foram integrados por merge normal em `14972d3`. Nenhuma alteração da worktree
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
compila o adaptador Java com Java 8 bytecode, gera dex com d8, alinha para 16 KiB
e assina novamente o APK. O empacotador verifica a Activity, `hasCode`, dex e
assinatura no artefato resultante.

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
- `src/android_images.rs`: reutiliza os arquivos compartilhados de decodificação,
  KittyHandler, cache e compositor RGBA. Usa `TerminalReader::spawn_with_graphics`
  com eventos ordenados; não duplica parser nem implementação dos protocolos.
  Cache Android limitado a 64 MiB e transmissão Kitty a 32 MiB, respeitando
  limites menores configurados. Um quadro pode reter pixels substituídos até
  sua apresentação; esses limites não são limites globais de PSS.
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

A barra oferece Esc, Tab, Ctrl de um uso e setas. Tocar na área do terminal
alterna o teclado; arrastar verticalmente rola o histórico. A composição fica
local até commit; o texto em composição recebe indicação visual. A área do
terminal acompanha o viewport informado pelo Android. Seleção, botões de
clipboard e aperfeiçoamentos de toque/mouse estão em implementação.

Não redistribuímos fontes do Android. Fontdue ainda não faz shaping de scripts
complexos nem emoji colorido. CJK pode carregar outlines grandes: o custo real
precisa ser medido. A fonte geométrica em `fonts/android-test.ttf` foi criada
para testes e não entra no APK.

O shell de sistema oferece comandos Android/toybox; não constitui um ambiente
completo de desenvolvimento. A rota SSH Rust empacotada está em implementação.
Android 10+ restringe execução de binários graváveis no HOME; instalar um pacote
Linux convencional ali não é uma solução demonstrada.

## Evidência e verificações pendentes

Medições/resultados abaixo têm escopo específico; não extrapolam para os
cenários ainda não executados.

| Evidência | Resultado observado |
| --- | --- |
| Isolamento Git | Worktree irmã, branch própria, commits/push normais sem trailer de coautoria. |
| PTY/runtime no macOS | 51 testes PTY e 4 runtime passaram, incluindo Ctrl-C, SIGWINCH, cwd/env privados e persistência de arquivos. |
| Atlas | 7 testes determinísticos passaram (A8, baseline, estilos, cache e limites). |
| Regressão compartilhada | 447 testes do binário macOS passaram após integrar imagens em `14972d3`, Rust 1.94.1. |
| Entrada Rust | 5 testes de IME e 6 do encoder compartilhado passaram em harness; os testes foram integrados ao binário para o CI. |
| APK inicial ARM64 | Build e assinatura executados localmente com SDK35/NDK27.1/Rust1.94.1. |
| APK com ponte IME | Build completo em `0b0e8a4`: launcher KokubanActivity, hasCode=true, classes.dex de 12.656 bytes, lib debug sem símbolos de 7.490.768 bytes; assinaturas v2/v3 e zipalign16 passaram. |
| Release CI ARM64 | Job de `4476798` passou build, Clippy e isolamento de dependências; artefato disponível no run [34001536192](https://github.com/guicybercode/kokuban.rs/actions/runs/34001536192). |
| AVD local `ygo` | Presente, Android35 ARM64. Inicialização terminou com falta de espaço; variante read-only também falhou. O emulador exige pelo menos 5 GB livres. Nenhuma execução local comprovada. |
| Instalação/shell/lifecycle CI x86_64 | Smoke em andamento; resultado ainda não registrado como aprovado. |
| IME real, toque, seleção/clipboard | Necessitam execução com teclado Android real; `adb input text` não prova composição. |
| SSH/ferramentas | Implementação e matriz de aplicações ainda incompletas. |
| Foto/animação/vídeo | Pipeline integrado; demonstração no dispositivo e medição ainda pendentes. |
| Consumo | PSS, CPU, latência e fluidez ainda sem medições aprovadas em runtime. Tamanho de build não prova baixo consumo. |

Ferramentas reproduzíveis:

```sh
python3 scripts/android/smoke.py --apk target/debug/apk/kokuban.apk --serial emulator-5554
python3 scripts/android/measure.py --scenario debug-idle --apk target/debug/apk/kokuban.apk --serial emulator-5554
```

O smoke registra saída por arquivo criado pelo shell dentro do sandbox, retoma
o mesmo processo/shell e verifica rotação. O coletor registra PSS e CPU do
processo principal e da árvore de subprocessos. Gfxinfo bruto não prova FPS do
softbuffer; latência permanece ausente até medição específica. Evidências
locais vão para `target/android-evidence`, sem credenciais ou dados privados.
Leia também `scripts/android/README.md`.

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
