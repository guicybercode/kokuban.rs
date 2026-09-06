# Compatibilidade de terminal

Comparação de 6 de setembro de 2026. Base do Kokuban: `7e0e9e2`.
Alacritty instalado: `0.17.0 (94e7c88)`. Claude Code instalado: `2.1.261`;
Codex CLI: `0.153.4`. O wrapper local do Kitty aponta para um aplicativo
ausente; as observações sobre ele vêm das fontes oficiais, não de uma execução
local ou de um benchmark.

## Comparação e prioridades

| Tema | Kitty | Alacritty | Kokuban e ação concreta |
| --- | --- | --- | --- |
| SSH e terminfo | O `kitten ssh` instala terminfo e integração no remoto; SSH comum pode precisar dessa preparação. | Distribui terminfo próprio. | Usa `xterm-256color`. Testar a capacidade no servidor e executar o `clear` real evita depender de terminfo específico do Kokuban. |
| Quadros de TUI | Preserva a cena durante modo 2026, com recuperação após 2 segundos. | Na versão 0.17.0, o parser vte 0.15 usa buffer limitado e timeout de 150 ms. | A base ignorava 2026. Implementar pausa de apresentação, consultas e liberação sem novos bytes; esse protocolo é usado no desenho do Codex. |
| Gráficos e imagens animadas | Protocolo inclui quadros parciais, composição e reprodução. | A proposta upstream de gráficos permanece aberta. | Linux já tem Kitty/Sixel e animação nativa. Metal rejeitava comandos de animação; compartilhar o motor de quadros e integrar o relógio de reprodução. |
| Abas e divisões | Recursos integrados. | São excluídos deliberadamente do escopo do projeto. | Divisões nativas no macOS; o estado de uma TUI deve congelar somente seu próprio painel. |
| Unicode | Tem tratamento de grafemas e protocolo opcional de largura explícita. | Preserva caracteres de largura zero junto da célula anterior. | A base perde acentos combinantes. Corrigir armazenamento/cópia/renderização exige cuidado além de ajustar o avanço do cursor. |

Fontes: [SSH do Kitty](https://sw.kovidgoyal.net/kitty/kittens/ssh/),
[recursos e escolhas de escopo do Alacritty](https://github.com/alacritty/alacritty/blob/v0.17.0/README.md#faq),
[recursos do Alacritty](https://github.com/alacritty/alacritty/blob/master/docs/features.md),
[proposta de gráficos](https://github.com/alacritty/alacritty/pull/4763),
[gráficos e animação do Kitty](https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation),
[largura de texto no Kitty](https://sw.kovidgoyal.net/kitty/text-sizing-protocol/),
[células e combinantes do Alacritty](https://github.com/alacritty/alacritty/blob/v0.17.0/alacritty_terminal/src/term/mod.rs).

O timeout de sincronização não é padronizado. As implementações comparadas
são [Kitty `screen_pause_rendering`](https://github.com/kovidgoyal/kitty/blob/master/kitty/screen.c)
e [vte 0.15](https://github.com/alacritty/vte/blob/v0.15.0/src/ansi.rs),
[dependência do Alacritty 0.17.0](https://github.com/alacritty/alacritty/blob/v0.17.0/alacritty_terminal/Cargo.toml).
O [protocolo](https://github.com/contour-terminal/vt-extensions/blob/master/synchronized-output.md)
separa processamento e apresentação. O [desenho do Codex](https://github.com/openai/codex/blob/main/codex-rs/tui/src/tui.rs)
usa `sync_update`.

Esta comparação não mede desempenho relativo. Xvfb comprova pixels e
interações, mas não representa scanout ou desempenho de uma GPU física.

## Correções e evidências

| Commit | Correção | Verificação |
| --- | --- | --- |
| `0c4de92` | RIS preserva medidas de célula e respostas de cores configuradas. | Consultas antes/depois do reset, inclusive bytes fragmentados. |
| `9c29c72` | macOS preserva notificações recebidas durante a renderização. | Barreira entre snapshot e conclusão; retry sem drawable; animação de diálogo. |
| `1ae8c0d` | DECSTR restaura modos de saída sem apagar conteúdo ou mover cursor. | Terminfo `xterm-256color`, atributos, margens, cursor, tela alternativa e histórico. |
| `34532d0` | macOS reconcilia seleção antes de copiar, arrastar e desenhar. | Clear, revisão de tela, histórico e troca de tela alternativa. |
| `2aa3f8c` | O smoke SSH executa o programa remoto `clear`. | Pixels, histórico, imagem e entrada posterior verificados pelo CI. |
| `e41e05b` | Metal aceita quadros, controle, composição e exclusão de animações Kitty. | 9 testes de imagem com dispositivo Metal real/API Validation e 12 testes do motor compartilhado. O relógio da janela ainda precisa ser integrado. |

O [CI de `2aa3f8c`](https://github.com/guicybercode/kokuban.rs/actions/runs/34051074018)
passou nos dois sistemas. No SSH, `clear` usou ncurses `6.4.20240113`, com
stdin/stdout/stderr no PTY e `TERM=xterm-256color`. A tela ficou inteiramente
na cor de fundo; onze capturas após novas tentativas de rolagem não mostraram
histórico ou imagem antigos. O texto `after-clear` chegou ao processo remoto.

O comportamento de reset segue a distinção documentada entre
[reset parcial e completo no xterm](https://invisible-island.net/xterm/manpage/xterm.html#VTxxx-Commands).
Executar apenas `printf '\033[2J'` não é evidência equivalente a executar
`clear`: o comando consulta terminfo e pode apagar também linhas salvas.

## Validação ainda em andamento

Atualização sincronizada completa, relógio de imagens Metal e interação com
Claude/Codex reais estão em implementação/teste. Inicialização ou `--version`
isolados não comprovam streaming, entrada, resize ou restauração do terminal.
As fixtures de modelo usam um servidor local controlado; isso permite testar
as TUIs sem uma chamada paga e sem herdar credenciais pessoais.
