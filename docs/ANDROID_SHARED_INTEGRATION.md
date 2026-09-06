# Handoff: integração dos componentes compartilhados Android

## Estado após a integração

Os merges normais `3775e6a` e `5638fab` incorporaram a base compartilhada e a
documentação Linux até `46e74fb`. O lockfile preserva as dependências das duas
plataformas e os dois pacotes winit; o teste PTY usa o novo argumento e o shell
portável. As adaptações Android foram concluídas em commits separados:

- `231509c`: seleção limitada, revisão de tela e acompanhamento da evicção.
- `7f9075e`: encoder compartilhado de paste, IDs assíncronos, revisão/modo,
  limite e recusa da fila sem encerrar a sessão.
- `3de3bb3`: dimensões físicas da área terminal e atualização só após ioctl
  bem-sucedido, incluindo mudanças sem alterar linhas/colunas.
- `1df1a00`: camadas Kitty atrás de fundos explícitos, visíveis através do
  fundo padrão. A otimização de opacidade permanece no renderer compartilhado.

Testes Rust cobrem evicção/limpeza, limites UTF-8, paste com controles,
respostas obsoletas e saturação da fila, além de resize somente em pixels.
O APK integrado `b158779` passou lifecycle, sete cenários de controles e
clipboard, aplicações SSH e seis cenários de mídia em debug/release,
incluindo as camadas negativas. As [evidências e medições](android-evidence/b158779/README.md)
registram perfis, hashes e limites. O teste estrito de resize remoto passou
em debug e release `c41bc56` após descartar uma leitura debug vazia. A composição
intermediária passou com Gboard real no fast `34008178897`, mas revelou glifos
CFF2 vazios; a correção visual exige novo APK. Periféricos USB físicos não foram testados.

A auditoria abaixo fica preservada como referência do contrato original.

## Auditoria original

Auditoria estática de **Android `4bc7b12` × Linux/vídeo `c02228d`**, base comum
`e8218a5`. As mudanças Linux auditadas estão em **main `cf2b9ac`**. A sessão
Android continua trabalhando em `codex/android-native`, na worktree irmã
`../kokuban-android`; esta auditoria não alterou essa worktree nem executou
merge ou build. HEADs posteriores precisam ser comparados antes da integração.

## Sequência para a sessão Android

1. Registre o HEAD atual e preserve suas alterações locais. Compare o que mudou
   desde `4bc7b12`, especialmente nos arquivos abaixo, antes de integrar
   `cf2b9ac` por merge normal. Não substitua arquivos inteiros pela versão de
   uma das branches e não reescreva o histórico publicado.
2. Resolva o lockfile e a incompatibilidade do teste PTY; mantenha as APIs
   compartilhadas e adapte os pontos de chamada Android indicados abaixo.
3. Execute os checks do resultado integrado, preserve os artefatos e atualize
   `docs/ANDROID.md` com o commit efetivamente testado e as limitações restantes.
   Faça commits pequenos e pushes normais, sem trailer de coautoria do Codex.

## Arquivos e contratos

- **`Cargo.toml` / `Cargo.lock`:** as duas branches inserem dependências no
  início da lista de `kokuban` (`android_logger` e `arboard`). Conserve ambas,
  com `arboard` restrito a Linux. Preserve os dois pacotes winit 0.30.13:
  `winit_android` aponta para `vendor/winit-0.30.13`; desktop usa o registry.
  Não transforme o ajuste Android em patch global nem atualize versões
  existentes para resolver o lockfile. O cliente SSH possui lockfile separado.
- **`src/pty/unix.rs`:** preserve o ambiente/cwd privado Android e a nova
  `resize_with_pixels(cols, rows, pixel_width, pixel_height)`. Android acrescentou
  `working_directory` como sexto argumento de `spawn_prepared`. O teste novo
  `pixel_aware_resize_is_visible_through_the_real_pty_winsize` precisa de `None`
  nessa posição e de `super::DEFAULT_SHELL` no lugar de `/bin/sh`.
- **`src/android_window.rs`, dimensões:** trocar o uso exclusivo de
  `resize(columns, rows)` pelo caminho com pixels. Use a área física de
  `controls.terminal`, excluindo toolbar, IME e margens externas; limite os
  valores ao formato `u16` do winsize. Atualize no primeiro desenho e nas
  mudanças de viewport, rotação ou DPI, inclusive quando a grade não mudar.
  Registre somente dimensões aplicadas com sucesso e evite ioctls repetidos.
  O cliente SSH já lê e encaminha `ws_xpixel`/`ws_ypixel`.
- **Seleção na janela Android:** as APIs já existem na base compartilhada.
  Use `selection::point_from_viewport`, `SelectionState::contains_cell` e
  `get_text_with_limit` com o orçamento Android de 64 KiB antes de construir
  uma cópia grande. Acompanhe `Grid::selection_revision()` para invalidar
  seleções e `rebase_after_eviction` para ajustar coordenadas quando o histórico
  descartar linhas. O contador relevante é a diferença entre
  `total_lines_pushed` e `scrollback_len()`; mudança de revisão tem precedência.
- **Clipboard/paste:** em `android_window.rs`, substitua texto cru entre
  marcadores por `input::paste::encode_paste`, com orçamento compatível com o
  limite Android e `TerminalWriter::max_nonfatal_input_bytes()`. Envie com
  `enqueue_nonfatal`, tratando recusa sem encerrar a sessão. Preserve a ponte
  Android; não use o backend arboard nessa plataforma. A resposta assíncrona
  precisa identificar a solicitação e validar `Grid::screen_revision()` e
  `bracketed_paste` antes do envio. Isso envolve `android_input.rs`
  (`ImeEvent::Clipboard`), `android_ime.rs` e `KokubanActivity.java`.
- **Animação/opacidade:** preserve `android_images.rs`, os módulos compartilhados
  de animação/store/snapshot e `advance_animations` com `WaitUntil`. Android já
  suspende timers sem superfície/foco. A opacidade e a remoção de desenhos
  totalmente cobertos são internas ao caminho compartilhado; não exigem outro
  decoder ou cache Android. Mantenha os limites Android e a ordem de locks
  grid → graphics. A diferença preexistente de camadas z negativas no desenho
  Android não é corrigida automaticamente por essa integração.

## Verificação e evidência

Build: Rust **1.94.1**, cargo-apk **0.10.0**, Java **17**, SDK/build-tools **35**,
NDK **27.1.12297006**. Use o wrapper `scripts/android/build.sh` e o workflow
`.github/workflows/android.yml`: ARM64 release e x86_64 debug/release, lockfiles
estáveis, isolamento de dependências, assinatura, dex e alinhamento ELF 16 KiB.
Execute também o CI desktop, incluindo os testes PTY e os smokes Linux.

No resultado integrado, teste seleção após evicção/limpeza/troca de tela,
clipboard com UTF-8, controles embutidos, limite e fila cheia, resposta de paste
atrasada e resize apenas em pixels. Repita lifecycle/IME/controles, SSH/TUIs,
foto/Sixel, animação após o emissor terminar e vídeo; registre consumo em
release com o mesmo cenário para permitir comparação.

Há [evidência preservada de `b9ba8b8`](https://github.com/guicybercode/kokuban.rs/blob/4bc7b12/docs/android-evidence/b9ba8b8/results.json)
no [run 34003726906](https://github.com/guicybercode/kokuban.rs/actions/runs/34003726906):
SSH com verificação de host, Neovim/tmux/fzf, Git/build Rust, foto, Sixel,
animação nativa e H.264 silencioso passaram no emulador API 35 x86_64, incluindo
release. Os resultados e medições foram preservados posteriormente em
`docs/android-evidence/`; esses resultados **não validam `4bc7b12` nem a futura
integração**. IME real, controles/clipboard e periféricos físicos permaneciam
sem aprovação documentada na auditoria. APK gerado e testes anteriores não
constituem conclusão da entrega Android.
