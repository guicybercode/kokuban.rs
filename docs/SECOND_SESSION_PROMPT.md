# Prompt para a segunda sessão: Android nativo

Copie o texto abaixo para uma segunda sessão do Codex. Ele atribui trabalho de implementação e validação em paralelo à sessão principal.

---

Trabalhe no Kokuban, terminal Rust do repositório `/Users/eguimacs/cybercode/kokuban.rs`. O objetivo completo do usuário é um terminal leve, utilizável em Linux e Android, com SSH, fotos, vídeo, animações e compatibilidade com aplicações modernas de desenvolvimento. Implemente o aplicativo Android nativo e publique cada pequena alteração validada. Continue além de planejamento, extração de biblioteca ou compilação: entregue um APK instalável e evidências de uso real.

## Divisão e isolamento

Você é responsável por entrada Android, ciclo de vida, teclado virtual/IME, toque, fontes, integração com o shell Android, empacotamento, documentação Android e verificações Android no CI. A sessão principal trabalha no Linux, imagens/Kitty, gráficos compartilhados e `src/terminal_reader.rs`.

Você não está sozinho no código. Preserve alterações de outras sessões. Inspecione `git status`, `git worktree list`, branches e remotos antes de agir. Leia `AGENTS.md` aplicável e as regras de worktree; nesta máquina o caminho encontrado é `~/skills/awesome-vibe-coding/cursor-claude-codex/coding/git-worktree-workflow.md` (o caminho global com `cursor-Codex-codex` estava ausente).

Crie uma worktree irmã em `../kokuban.rs-android-native` com branch `codex/android-native`, baseada no `origin/main` atualizado. Se já existir, inspecione e reutilize a worktree correta sem apagar trabalho. Execute todos os comandos de desenvolvimento nessa worktree; não altere a branch da sessão principal.

Coordene alterações em `src/main.rs`, `Cargo.toml`, `Cargo.lock`, `src/pty/unix.rs`, `src/glyph_atlas.rs` e arquivos compartilhados antes de refatorações grandes. Prefira módulos Android específicos e interfaces pequenas. Não copie o terminal inteiro para criar uma implementação divergente. Se as sessões não tiverem um canal direto, registre contratos e alterações necessárias em `docs/ANDROID.md`, mantenha commits fáceis de integrar e abra um PR de rascunho; continue todo o trabalho independente enquanto a integração é resolvida.

Faça commits pequenos e coerentes, rode as verificações adequadas e dê push de cada commit para `origin/codex/android-native`. Use push normal; nunca force push nem descarte alterações alheias. O usuário já autorizou esses pushes. Não comite credenciais, arquivos de assinatura privados ou artefatos locais de SDK. Entregue os hashes e o resultado das verificações. A integração final precisa preservar as mudanças Linux e passar pelas verificações das plataformas existentes.

## Evidência inicial a revalidar

Na auditoria de 2026-09-05, baseada em `9fb2ea7`:

- `src/main.rs` habilitava somente macOS e Linux e tinha `compile_error!` nos demais alvos. Não havia `src/lib.rs`, entrada Android nem metadados de APK.
- O Linux já usava winit 0.30.13 e softbuffer 0.4.8. `src/linux_window.rs` continha janela, desenho por CPU, PTY, teclado/IME e eventos de mouse. O loop usava `ControlFlow::Wait`. Havia `resumed`, mas não implementação de `suspended` nem tratamento de `WindowEvent::Touch`.
- Softbuffer 0.4.8 já possui `src/backends/android.rs` no código da dependência. É um caminho concreto para reutilizar o desenho por CPU; não exige adicionar um motor web. Essa implementação aloca e converte um quadro completo, portanto seu custo precisa ser medido.
- `font-kit` 0.14.3 escolhe `FsSource` e `/system/fonts` no Android, porém seu manifesto também inclui `freetype-sys` e `yeslogic-fontconfig-sys` nesse alvo. Evite transportar dependências nativas de desktop apenas para descobrir fontes. Implemente uma opção Android com rasterização Rust e fontes acessíveis no dispositivo, com fallback e licença adequada caso embutidas.
- `src/pty/unix.rs` escolhia `KOKUBAN_SHELL`, depois `SHELL`, depois `/bin/sh`; tratava EOF via `EIO` somente sob `cfg(target_os = "linux")`. Verifique os comportamentos Android e use um shell realmente disponível, incluindo `/system/bin/sh` como fallback Android. Teste permissões, sessão, sinais, resize, encerramento e subprocessos no dispositivo.
- `src/config.rs` carregava somente `kokuban.toml` do diretório atual. No Android, use armazenamento privado da aplicação e não dependa do diretório inicial do processo.
- O CI existente verificava macOS e Linux, incluindo primeiro quadro Linux sob Xvfb. Isso não prova execução Android nem qualidade de vídeo.

Ambiente encontrado: SDK em `~/Library/Android/sdk`, NDK `27.1.12297006`, `adb` disponível, alvos Rust `aarch64-linux-android` e `x86_64-linux-android` instalados em ao menos uma toolchain, AVD `ygo` com imagem Android 35 arm64. `adb devices -l` não encontrou dispositivo conectado; a presença do AVD ainda não prova que ele inicializa. `cargo apk` não estava instalado. Confira a toolchain antes de compilar: o `cargo` do PATH vinha do Homebrew; o CI usa Rust 1.94.1. Use `rustup run 1.94.1 cargo ...` para corresponder ao CI e instale o alvo nessa toolchain se necessário. Não sobrescreva `HOME` ou `CODEX_HOME`.

## Trabalho a executar

1. Implemente a entrada nativa e o build Android. Comece com winit `android-native-activity`, uma biblioteca `cdylib` e `android_main(AndroidApp)` usando o tipo reexportado pelo winit. A documentação oficial explica esse contrato e como evitar versões conflitantes do glue: [winit Android](https://docs.rs/winit/latest/winit/platform/android/). Registre os comandos reproduzíveis para gerar e instalar o APK. Não considere remover o `compile_error!` como suporte Android concluído.
2. Faça a janela apresentar o terminal real. Reutilize o núcleo, o desenho e o contrato de atualização existentes. Crie/libere as superfícies nos eventos de ciclo de vida corretos; pause a apresentação sem perder a sessão durante suspensão. Teste retomada, rotação e mudanças de tamanho repetidas, sem segurar uma superfície inválida. Implemente tratamento de erro visível e logs úteis.
3. Resolva fontes e inicialização do PTY Android, configuração e diretório gravável. Demonstre prompt funcional, execução de comando e saída de shell. Não apresente o shell básico de sistema como ambiente completo de desenvolvimento.
4. Entregue uso por toque e teclado: abrir/fechar teclado virtual, composição e commit de texto, acentos, apagar, Enter, teclas externas, Ctrl, Esc, Tab e setas acessíveis no telefone. Adicione seleção/cópia/cola e rolagem com gestos sem quebrar mouse/teclado externos. Verifique composição com um IME real, não somente `adb shell input text`.
5. Integre gráficos compartilhados conforme chegarem da sessão principal. Demonstre foto, atualizações animadas e vídeo por uma rota de aplicação/protocolo documentada. Preserve limites de memória e responsividade. Acrescente apenas dependências necessárias; meça antes de substituir toda a renderização.
6. Entregue uma rota de SSH utilizável no Android, com autenticação e validação de chave do servidor, entrada interativa, resize e desconexão. Verifique como disponibilizar ferramentas locais de desenvolvimento sob as restrições reais da plataforma. Documente o que roda localmente e por SSH, com versões e comandos reproduzíveis. Um cliente SSH de exemplo que ignora a identidade do servidor não satisfaz a entrega.
7. Adicione build Android ao CI e testes adequados. Faça install/launch/smoke em emulador ou dispositivo e registre screenshots/logs/resultados sem dados privados. Verifique também a integração com Linux/macOS depois de mudanças compartilhadas.

## Critério de entrega

Mantenha `docs/ANDROID.md` com build, instalação, atalhos, limitações e evidências. O APK deve abrir e aceitar entrada real, executar comandos, sobreviver a suspensão/retomada e rotação, acessar SSH e apresentar mídia. Teste aplicações de desenvolvimento reais, por exemplo editor de terminal, multiplexer, seletor interativo, Git e execução de um projeto. A lista representa cenários mínimos de verificação, não redefine o objetivo como suporte apenas a essas aplicações.

Registre tamanho de APK/biblioteca em release, memória ociosa e com mídia, CPU ociosa, latência de entrada e fluidez de quadros, sempre com dispositivo, versão e cenário. “Escrito em Rust” não demonstra baixo consumo. Identifique FFI e bibliotecas externas e mantenha a lógica da aplicação em Rust.

Não declare conclusão com `cargo check`, APK apenas gerado, tela estática ou testes unitários isolados. Se um dispositivo não estiver disponível, execute os demais trabalhos e registre exatamente a validação ausente. Nunca fabrique resultados. Informe o que foi entregue, onde está o APK/PR, commits publicados e o próximo requisito ainda incompleto.
