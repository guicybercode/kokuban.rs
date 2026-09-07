# Compatibilidade de terminal

Comparação de 6 de setembro de 2026. Base inicial do Kokuban: `7e0e9e2`;
estado validado: `eee48ba`.
Alacritty instalado: `0.17.0 (94e7c88)`. Claude Code instalado: `2.1.261`;
Codex CLI: `0.153.4`. O wrapper local do Kitty aponta para um aplicativo
ausente; as observações sobre ele vêm das fontes oficiais, não de uma execução
local ou de um benchmark.

## Comparação e prioridades

| Tema | Kitty | Alacritty | Kokuban e ação concreta |
| --- | --- | --- | --- |
| SSH e terminfo | O `kitten ssh` instala terminfo e integração no remoto; SSH comum pode precisar dessa preparação. | Distribui terminfo próprio. | Usa `xterm-256color`; o CI executa `clear` real no servidor SSH e verifica tela, histórico e entrada posterior. O servidor precisa conhecer esse terminfo. |
| Quadros de TUI | Preserva a cena durante modo 2026, com recuperação após 2 segundos. | Na versão 0.17.0, o parser vte 0.15 usa buffer limitado e timeout de 150 ms. | Agora reconhece e consulta 2026, mantém respostas de protocolo durante a pausa e recupera apresentação após 2 segundos sem depender de novos bytes. |
| Gráficos e imagens animadas | Protocolo inclui quadros parciais, composição e reprodução. | A proposta upstream de gráficos permanece aberta. | Linux tem Kitty/Sixel e animação nativa. Metal agora compartilha o motor de quadros, aceita controles de animação e avança imagens visíveis pelo timer da janela, mesmo sem saída do PTY. |
| Abas e divisões | Recursos integrados. | São excluídos deliberadamente do escopo do projeto. | Divisões nativas no macOS; o cache de cena mantém cada painel sincronizado separado dos demais. |
| Unicode | Tem tratamento de grafemas e protocolo opcional de largura explícita. | Preserva caracteres de largura zero junto da célula anterior. | Preserva combinantes no armazenamento, histórico e cópia, até 64 bytes UTF-8 adicionais por célula; Linux/Metal compõem acentos no desenho e sobrepõem marcas restantes. Isso não implementa todos os grafemas ou larguras de emoji. |

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
| `e41e05b` | Metal aceita quadros, controle, composição e exclusão de animações Kitty. | Execução local: 9 testes de imagem com dispositivo Metal real/API Validation e 12 testes do motor compartilhado; inclui pixels, limites e falhas de alocação. |
| `7f959d6` | Atualização sincronizada com consultas, recuperação por timeout e preservação da cena. | Parser fragmentado, respostas durante pausa, EOF, timer sem novos bytes, isolamento por painel e smoke de pixels Linux com resize. |
| `5286c80` | Responde consultas de estado e posição do cursor usadas por aplicações interativas. | `CSI 5 n`, `CSI 6 n` e `CSI ? 6 n`, inclusive fragmentação; variantes privadas não implementadas continuam ignoradas. |
| `8498e17` | O timer macOS avança animações visíveis sem novas leituras do PTY. | Integração com a ordem de locks do leitor; testes de visibilidade parcial, área de conteúdo e imagens ocultas. |
| `6b46f4e` | Trunca caminhos da barra de estado em limites válidos de Unicode. | Caminhos multibyte e larguras reduzidas deixam de causar panic ao redimensionar. |
| `2b22c73` | Mantém três conjuntos de buffers Metal até a GPU terminar cada submissão. | Fila GPU bloqueada deliberadamente, três quadros distintos, quarto quadro adiado e leitura dos pixels após liberação. |
| `e475aab` | Preserva texto combinante entre células, histórico, seleção e renderizadores. | Inserção/exclusão, erase, resize, cópia exata e snapshots; readback Metal compara acentos compostos/decompostos e verifica sobreposição sem apagar o glifo-base. |
| `37ba8c9` | Limita combinantes a 64 bytes adicionais por célula antes de alocar ou copiar snapshots. | 284 testes locais do núcleo passaram, incluindo excesso de 10 mil marcas, integridade UTF-8 e recuperação após escrita e apagamento. |
| `eee48ba` | Testa os clientes reais com estado descartável e respostas locais em streaming. | Claude Code 2.1.261 e Codex 0.153.4: texto editado na janela, streaming parcial, resize, resposta completa, saída e entrada posterior. |

O CI de `eee48ba` passou em Linux e macOS, tanto no
[push](https://github.com/guicybercode/kokuban.rs/actions/runs/34071403430)
quanto na [PR](https://github.com/guicybercode/kokuban.rs/actions/runs/34071405675):
`cargo check`, testes de todos os alvos e Clippy.
No Linux também passaram os smokes gráficos de sincronização, imagens,
clipboard, vídeo curto com mpv e aplicações reais por SSH. O smoke de
sincronização preserva a cena anterior durante uma atualização dividida,
inclui resize e libera a cena por término explícito ou timeout.

Os testes locais de Metal usaram um dispositivo real e leitura dos pixels das
texturas. Cobrem buffers em uso pela GPU, preservação de cena durante pausa e
mudança de fonte, e os casos de acentos descritos acima. Os testes Metal no CI
podem retornar sem executar a parte gráfica quando não há dispositivo GPU;
um job verde sozinho não comprova essa execução. O avanço da animação no timer
da janela está implementado, mas ainda não tem um smoke macOS de janela real
equivalente ao smoke gráfico Linux. A evidência de leitura de textura não deve
ser apresentada como esse teste de janela.

O [CI de `2aa3f8c`](https://github.com/guicybercode/kokuban.rs/actions/runs/34051074018)
passou nos dois sistemas. No SSH, `clear` usou ncurses `6.4.20240113`, com
stdin/stdout/stderr no PTY e `TERM=xterm-256color`. A tela ficou inteiramente
na cor de fundo; onze capturas após novas tentativas de rolagem não mostraram
histórico ou imagem antigos. O texto `after-clear` chegou ao processo remoto.

O comportamento de reset segue a distinção documentada entre
[reset parcial e completo no xterm](https://invisible-island.net/xterm/manpage/xterm.html#VTxxx-Commands).
Executar apenas `printf '\033[2J'` não é evidência equivalente a executar
`clear`: o comando consulta terminfo e pode apagar também linhas salvas.

## Comportamento e limites

Atualização sincronizada de TUI e animação nativa de imagens são mecanismos
distintos. O modo 2026 congela a apresentação enquanto o parser continua
processando texto, imagens e consultas. No Linux, o gate protege também
redraws de resize, seleção, IME e timers. No macOS, cada cena retém seus
vértices e texturas anteriores; outros painéis continuam desenhando. O prazo
de 2 segundos não é renovado por inícios repetidos dentro da mesma pausa.
Um quadro remoto que ultrapasse esse prazo pode ser exibido incompleto para
permitir recuperação de aplicações que não enviam o término da atualização.

O suporte a combinantes retém até 64 bytes UTF-8 adicionais por célula, além
do escalar-base. Uma marca que ultrapasse esse limite é ignorada por inteiro;
o limite é verificado antes de alocar ou copiar uma cauda compartilhada com
um snapshot. Isso limita memória e trabalho de normalização quando um
produtor envia marcas continuamente sem avançar o cursor. Substituir ou
apagar a célula libera a cauda e permite receber novas marcas.

A cópia preserva a sequência retida, sem normalização; NFC é usada somente
para desenhar. Sequências de emoji com ZWJ, seletores que alteram largura,
shaping complexo e reconstrução de linhas com soft-wrap não estão comprovados
por esses testes.

## Claude Code e Codex CLI

Os dois clientes reais passaram no Linux, dentro da janela do Kokuban sob
Xvfb, com teclado/mouse XTest e PTY encaminhado por SSH com chave de host
verificada. As versões são fixadas pelo CI: Claude Code **2.1.261** e Codex
CLI **0.153.4**.

| Cenário | Codex | Claude Code |
| --- | --- | --- |
| Digitar, apagar um caractere e conferir `compat-input-42` na tela antes de enviar | Passou | Passou |
| Receber resposta parcial sem antecipar o trecho final | Passou | Passou |
| Redimensionar de 100×30 para 88×26 durante o streaming e conferir texto/pixels | Passou | Passou |
| Mostrar a resposta completa, com cada marcador uma vez no texto selecionado | Passou | Passou |
| Sair normalmente, restaurar atributos do PTY, executar `clear` e receber `after-tui` | Passou | Passou |

Cada execução fez uma única solicitação de modelo ao servidor local de teste.
O servidor verifica o texto digitado separadamente dos lembretes de contexto
adicionados pelo cliente. O teste usa digitação a 150 ms por caractere; não é
um teste de desempenho de entrada rápida. O código reproduzível está em
[`linux-ai-tui-smoke.py`](../scripts/linux-ai-tui-smoke.py), com instalação das
versões e dependências definida no [workflow](../.github/workflows/ci.yml).

O estado dos clientes é novo e descartável. O Claude inicia com onboarding
preparado e confiança somente na pasta temporária gerada; nenhuma configuração
ou credencial pessoal é copiada. As respostas são SSE determinísticas de
localhost, sem chamada paga. Isso valida a interação dessas TUIs com o terminal,
sem avaliar qualidade de modelos ou outros provedores e fluxos de autenticação.

Os resultados de clear, sincronização e clientes reais, junto do commit e hash
do binário testado, estão preservados em
[`terminal-compatibility-results.json`](terminal-compatibility-results.json).
Os artefatos dos runs também contêm capturas e registros limitados do teste;
sua retenção no CI é de sete dias.

## Integração Android

Android continua numa frente separada. As mudanças do núcleo exigem adaptação
da aquisição de cena, expiração sem novos bytes e snapshots de `Cell` no
renderizador Android, conforme o [contrato de integração](ANDROID_SHARED_INTEGRATION.md).
O CI desktop desta comparação não valida um APK com essas novas correções.
