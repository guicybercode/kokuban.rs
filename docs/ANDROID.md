# Android nativo

Frente de implementação em `codex/android-native`, na worktree
`../kokuban-android`, baseada em `origin/main` (`62bce8e`). A sessão Linux mantém
`linux_window.rs`, `terminal_reader.rs`, `software_raster.rs` e `renderer/`.

## Contratos de integração

- A entrada Android será `android_main(AndroidApp)` em uma biblioteca `cdylib`,
  usando o tipo reexportado por winit e `android-native-activity`.
- O núcleo VT, parser, grid, cores, seleção, compositor A8 e filas de PTY são
  compartilhados. Android terá janela/lifecycle e atlas próprios; não haverá
  cópia dos protocolos Kitty/Sixel. A ingestão de mídia aguarda o contrato
  compartilhado publicado pela sessão Linux.
- Fontes Android usam rasterização Rust (`fontdue`) e fontes do sistema. O alvo
  Android não deve carregar font-kit, FreeType ou fontconfig. APIs Android,
  libc/PTY, JNI e janelas continuam sendo FFI do sistema.
- Armazenamento, configuração e HOME usam o diretório privado fornecido pela
  Activity; ambiente e diretório de trabalho são passados somente ao filho PTY.
- Suspensão libera Surface/Context antes de retornar ao Android, preservando
  sessão e grid. Retomada recria recursos gráficos. O loop espera eventos em
  repouso; as medições devem conferir o custo dos leitores e da apresentação.
- Mudanças compartilhadas serão pequenas e integráveis por commits normais,
  preservando os commits Linux. Push incremental sem force-push e sem trailer
  de coautoria Codex.

## Evidência inicial (2026-09-05)

O SDK existe em `~/Library/Android/sdk`, com NDK `27.1.12297006`; Java é
17.0.19. A toolchain de referência é Rust 1.94.1. O AVD `ygo` existe, porém a
tentativa de inicialização terminou com `FATAL: ... insufficient disk space`.
O volume tinha cerca de 200 MiB livres. A instalação do alvo Android na
toolchain 1.94.1 também falhou com `No space left on device`, com rollback.
Isso não constitui evidência de execução Android.

Ainda não há APK ou validação de terminal, IME, SSH, mídia, aplicações de
desenvolvimento ou consumo. Os passos seguintes são implementar, compilar,
instalar e registrar os cenários do `SECOND_SESSION_PROMPT.md`, mantendo
resultados efetivamente executados separados das verificações pendentes.

Referências: [winit Android](https://docs.rs/winit/latest/winit/platform/android/),
[android-activity](https://github.com/rust-mobile/android-activity),
[cargo-apk](https://github.com/rust-mobile/cargo-apk).
