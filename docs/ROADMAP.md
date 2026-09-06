# Entrega Linux e Android

Objetivo do usuário: um terminal leve, utilizável em Linux e Android, com SSH, fotos, vídeo, animações e aplicações modernas de desenvolvimento, mantendo a implementação da aplicação em Rust. Cada pequena alteração deve ser versionada e enviada ao remoto. A segunda sessão recebe uma frente de implementação no [prompt Android](SECOND_SESSION_PROMPT.md).

Esta matriz registra a auditoria inicial de 2026-09-05, em `9fb2ea7`. Atualize as linhas com commits e evidência de execução conforme o trabalho avançar. Código existente e configuração de CI indicam capacidades e intenção de teste; não substituem resultados executados.

## Android na branch de integração

A branch `codex/android-native`, PR [#8](https://github.com/guicybercode/kokuban.rs/pull/8), contém o APK NativeActivity, PTY/shell Android, fontes fontdue, entrada/toque, clipboard e cliente SSH Rust. Em `b9ba8b8`, o emulador Android 15/API 35 x86_64 executou SSH com verificação da identidade do host, Neovim, tmux, fzf, Git e build/run Rust, em debug e release. Foto, Sixel, animação Kitty após o emissor encerrar e vídeo H.264 silencioso por SSH tiveram verificação de pixels. [Resultados, APK e medições](ANDROID.md) identificam exatamente as versões testadas.

O APK integrado `b158779` passou sete cenários de controles/clipboard, lifecycle, aplicações SSH e seis cenários de mídia, incluindo as camadas Kitty negativas. A medição release registrou 0,2% de um núcleo em repouso, PSS do app de 25.586→29.712 KiB e mediana de 41,08 ms no cenário controlado de eco. O Gboard real passou `café` por toque e composição intermediária Hangul no teste posterior `34008178897`; o glifo CFF2 vazio revelou uma correção visual ainda necessária no fallback. O resize SSH passou com validação estrita em debug e release `c41bc56`, após descartar um resultado debug anterior que leu o arquivo vazio. Os merges normais `3775e6a` e `5638fab` incorporaram a base Linux e sua documentação até `46e74fb`. Esta evidência da branch não declara suporte Android concluído ou já integrado na `main`.

## Primeira medição Linux release em 2026-09-05

O código `29ac690`, publicado na `main`, passou no [workflow de medição](https://github.com/guicybercode/kokuban.rs/actions/runs/34005940902) e no [CI completo](https://github.com/guicybercode/kokuban.rs/actions/runs/34005916270), com **582 testes Linux e 484 macOS**. O executável release sem strip adicional ficou em 7,98 MiB; o repouso usou 11,93 MiB de RSS. A saída de 2,5 MiB foi processada com resposta final em 0,126 s, retendo 27,73 MiB após preencher o histórico. O vídeo 320×180 apresentou os 72 quadros e usou 8,75% de um núcleo do terminal, com RSS máximo amostrado de 28,70 MiB.

Os [dados, hardware, dependências e limites da medição](LINUX_PERFORMANCE.md) estão preservados. O ioctl real passou na comparação com a janela estabilizada: 80×24 células e 720×408 pixels. Isso fornece uma referência de consumo Linux/X11 em cenário curto; não comprova desempenho prolongado, Wayland, áudio, autodimensionamento do mpv ou todos os dispositivos. A integração Android continua na frente paralela, com [ajustes compartilhados documentados](ANDROID_SHARED_INTEGRATION.md). Grafemas compostos e reconstrução de soft-wrap ainda precisam de implementação; o armazenamento atual de um escalar por célula não preserva todas as sequências Unicode.

## Vídeo com mpv verificado em 2026-09-05

O código `c02228d`, integrado e publicado na `main`, passou no [CI Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34005117373): **582 testes Linux e 484 macOS**, verificações anteriores e reprodução real com mpv 0.37.0. O clipe FFV1 320×180 de seis segundos apresentou todos os 72 quadros identificáveis, sem capturas inválidas; Space pausou e retomou, o último quadro permaneceu estável e os processos encerraram normalmente.

- `e4f961a`: dimensões físicas no PTY, incluindo mudanças somente em pixels, com teste de ioctl real.
- `6300ade`: deixa de desenhar imagens totalmente encobertas, preservando dados/placements e composição transparente.
- `e4a455b`: mpv/ffmpeg reais, verificação dos pixels, pausa e medições separadas de terminal/player.

O [relatório de vídeo](LINUX_VIDEO.md) preserva JSON e screenshots. Essa primeira medição foi em debug e não comprova áudio, vídeo por SSH, Wayland, resize durante reprodução ou uso prolongado. A medição release posterior está registrada acima.

## Clipboard, seleção e aplicações SSH verificados em 2026-09-05

O código `f4c7330`, integrado e publicado na `main`, passou no [CI Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34005044694): **566 testes Linux e 480 macOS**, check, Clippy, imagens/animação e testes de janela real. A sessão SSH com chave do servidor fixada rejeitou a chave incorreta e validou autenticação, teclado, resize remoto, Ctrl+C, edição Unicode no Neovim 0.9.5, seleção no fzf 0.44.1, divisão/comando no tmux 3.4 e desconexão normal.

- `e8218a5`: seleção limitada por coordenadas e bytes, com revisão de tela e acompanhamento do descarte de histórico.
- `d261466` e `219f66d`: clipboard fora da thread da janela, paste limitado e integração dos atalhos/seleção Linux.
- `26d776d`: teste XTest de UTF-8, cópia com destaque visual e retenção do clipboard, colagem e seleção local com Shift durante mouse SGR.
- `fca890e`, `1c3e13b` e `f4c7330`: sessão OpenSSH real e fixtures de autenticação/teclado estáveis. Detalhes e versões em [validação de aplicações Linux](LINUX_APPS.md).

A evidência é Linux/X11; Wayland, IME completo, soft-wrap/grafemas compostos, OSC 52 e outros aplicativos continuam pendentes. A reprodução com mpv tem evidência própria na seção acima. As seções abaixo preservam os resultados das entregas anteriores.

## Animação nativa verificada em 2026-09-05

O código `ab40737`, integrado e publicado na `main`, passou no [CI Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34002541075): **533 testes Linux e 462 macOS**, check de todos os alvos, Clippy e isolamento de dependências. O teste Xvfb confirmou imagens estáticas e, adicionalmente, reprodução nativa com pixels herdados entre frames, duas repetições completas e manutenção do último frame por mais de um ciclo após o emissor parar de escrever.

- `82773bc` e `4e9c5b5`: Base64 sem padding, blocos codificados de 128 KiB e comandos tipados de animação.
- `b10a890` e `f1ebd9e`: canvases limitados por memória, composição/edição/exclusão de frames e respostas de protocolo, incluindo seleção por número `I`.
- `17484b3` e `45fa644`: agendamento apenas para imagens visíveis no Linux e verificação dos pixels apresentados.
- `ab40737`: exemplo `cargo run --example graphics -- animate` e [contrato de integração Android](ANIMATION.md).

O teste visual usa uma sequência sintética compatível com o formato do icat; não executou o binário icat nem mediu desempenho. A reprodução nativa foi comprovada no Linux/X11. Vídeo geral, áudio, experiência Wayland, SSH e aplicações reais, clipboard/seleção Linux e consumo de recursos continuam exigindo trabalho e evidência. Metal retorna `ENOTSUP` para animação.

A segunda sessão está ativa na worktree `../kokuban-android`, branch `codex/android-native`; nela já há commits de aplicação, gráficos, entrada e SSH Android. Esses commits ainda não foram integrados nesta `main` nem validados aqui em dispositivo. A integração e a evidência Android posteriores estão registradas na seção da branch acima.

## Primeira validação de imagens em 2026-09-05

O código em `5849e76`, publicado na `main`, passou no [CI Linux/macOS](https://github.com/guicybercode/kokuban.rs/actions/runs/34001238053): **497 testes Linux e 430 macOS**, check de todos os alvos, Clippy, isolamento de dependências e primeiro quadro Linux sob Xvfb. O novo teste `scripts/linux-graphics-smoke.py` verificou pixels apresentados de Kitty PNG e Sixel, substituição vermelho→verde e ambos os protocolos desabilitados. A existência de uma janela não basta para esse teste passar.

- `a0050a4`: decoder RGB/RGBA/PNG compartilhado e cache CPU limitado por bytes e 4096 imagens.
- `94952b6`: eventos gráficos ordenados no leitor do PTY, com respostas fora do lock do grid.
- `24c8333`: composição RGBA com escala, transparência e recorte.
- `defbec8`: imagens acompanham texto e histórico, com isolamento da tela alternativa. Imagens que cruzam margens parciais são descartadas, uma limitação ainda documentada.
- `8a272eb`, `826275e` e `5849e76`: Linux anuncia e renderiza os protocolos habilitados, aplica camadas Kitty e desempata pelo ID do cliente; testes integrados cobrem upload, respostas, exclusão, retransmissão, cache e composição.
- `e08c992`: exemplo Rust para PNG, Sixel e sequência de 120 quadros; README distingue recursos por plataforma.
- `ee01e9a`: validação gráfica Xvfb reproduzível no CI.

Esses resultados comprovam imagens estáticas e substituição de quadros no Linux/X11. Não comprovam reprodução geral de vídeo, áudio, animação nativa Kitty, FPS sustentado, baixo consumo, experiência Wayland, SSH completo, compatibilidade abrangente de aplicações ou Android. O exemplo `stream` solicita 30 FPS, mas ainda não foi medido como benchmark. A compilação do exemplo passou no CI; sua presença não substitui teste de reprodução.

O prompt da segunda sessão foi entregue ao usuário e publicado em `62bce8e`; a preferência por commits sem Codex como coautor foi registrada em `f38e4ab`. Não há evidência de execução da segunda sessão nesta auditoria. O próximo trabalho continua sendo entregar e verificar todos os requisitos abaixo; a matriz inicial fica preservada como referência.

As compilações locais encontraram falta de espaço em disco. Foram removidos artefatos de `target/` e duas extrações de dependências Cargo gerados nesta sessão; a validação Linux foi feita no CI. Antes de compilar o APK, confira novamente espaço livre além de SDK/NDK.

## Matriz inicial e critérios restantes

| Requisito | Evidência inicial | Falta para comprovar entrega |
| --- | --- | --- |
| Linux utilizável | `src/main.rs`, `src/linux_window.rs`: janela winit X11/Wayland, desenho por CPU, PTY, entrada/IME e rolagem. `.github/workflows/ci.yml`: checks e primeiro quadro Xvfb configurados. | Testar sessão interativa real em X11 e Wayland, fontes/DPI, clipboard, seleção, resize, encerramento, instalação e configuração. Registrar resultados por versão. |
| Android nativo utilizável | `src/main.rs` rejeita Android; não existe entrada Android, biblioteca `cdylib` ou build APK. SDK/NDK e AVD estão presentes na máquina de desenvolvimento. | Gerar, instalar e executar APK; implementar fontes, PTY, armazenamento, lifecycle, IME, toque, atalhos, seleção e clipboard; testar rotação e retomada. AVD ainda não inicializado nesta auditoria. |
| SSH em ambas as plataformas | PTY pode executar comandos do shell; `src/pty/unix.rs` define ambiente de terminal. | Demonstrar cliente disponível, autenticação, identidade do servidor, sessão remota interativa, resize, desconexão e aplicações remotas. A disponibilidade do comando `ssh` no computador não comprova a rota Android. |
| Fotos | Há parser e implementação de gráficos em `src/parser/kitty_graphics.rs`, `src/parser/sixel.rs` e `src/renderer/`; o renderer atual é habilitado para macOS. Linux inicializa o PTY com gráficos desabilitados em `src/linux_window.rs`. | Integrar ingestão, armazenamento e composição de imagens ao Linux e Android; testar clientes reais, fragmentação, transparência, posicionamento, resize e limites de memória. |
| Animações | A estrutura de gráficos existente precisa de validação específica por plataforma. | Verificar o protocolo anunciado, agendamento de quadros, atualização/deleção de imagens, pausa/retomada e consumo de CPU em repouso. Exibir sequência animada reproduzível. |
| Vídeo | Nenhum teste de reprodução foi executado na auditoria inicial. | Definir e implementar a rota de reprodução, validar com conteúdo e aplicação reais em Linux e Android, medir resolução, FPS, duração, memória e responsividade. Registrar suporte a áudio e sincronização quando fizerem parte da rota. Foto estática não prova vídeo. |
| Aplicações modernas de desenvolvimento | Núcleo VT/ANSI, PTY e entrada existem; não há evidência suficiente de compatibilidade abrangente. | Manter uma matriz de aplicações/versões e executar editor, multiplexer, seletor, Git, CLI interativa e build/run de projeto. Cobrir Unicode, cores, mouse, teclado, colagem e resize. Verificar ferramentas locais e por SSH; corrigir incompatibilidades observadas. |
| Leve e Rust | Código da aplicação é Rust. `Cargo.toml` inclui APIs nativas Apple e, no Linux, font-kit com FreeType/fontconfig; winit e softbuffer interagem com o sistema. | Medir tamanho em release, dependências, memória, CPU ociosa, entrada, throughput e mídia nas plataformas alvo. Reduzir custos identificados e registrar limites. Não descrever o conjunto de dependências como inteiramente Rust enquanto houver bibliotecas C e APIs do sistema. |
| Versionamento incremental | Repositório Git e CI por push/PR existem. | Commit e push de cada alteração coerente após verificação apropriada; registrar hash, branch e resultado. Não acumular toda a implementação num único commit. |
| Segunda sessão | [Prompt de execução Android](SECOND_SESSION_PROMPT.md) define worktree, responsabilidade, coordenação e validação. | Entregar o prompt ao usuário; a existência do documento não significa que uma segunda sessão já executou ou concluiu a tarefa. |

## Frentes de implementação

1. **Linux e núcleo compartilhado, sessão principal:** ligar a rota de imagens ao desenho Linux, manter respostas de protocolo coerentes com o que realmente funciona, controlar alocações e backlog, e validar fotos e quadros atualizados. Depois completar vídeo/animação, entrada/clipboard e matriz de aplicações. As interfaces compartilhadas devem permanecer reutilizáveis pelo Android.
2. **Android nativo, segunda sessão:** worktree `codex/android-native`; APK, entrada/lifecycle, fontes, PTY, IME/toque, instalação e CI. Integrar as capacidades compartilhadas, testar SSH e ferramentas reais e apresentar o resultado em emulador/dispositivo.
3. **Integração e medição:** incorporar os commits preservando trabalho simultâneo, executar os cenários por plataforma e publicar resultados reproduzíveis. Um build multiplataforma verde não substitui essa etapa.

Coordene mudanças de manifesto, entrada, PTY e atlas entre as sessões. Não duplique todo o terminal nem desative recursos existentes apenas para fazer o build passar. Registre bloqueios concretos e continue os trabalhos independentes.

## Como demonstrar baixo consumo

Use a mesma configuração, conteúdo e dimensões ao comparar commits. Registre hardware, sistema, backend, versão Rust, perfil release e comando de medição. Defina metas quantitativas a partir da medição inicial e do dispositivo alvo; ainda não há metas aprovadas nem resultados nesta auditoria.

| Cenário | Medições necessárias |
| --- | --- |
| Instalação e início | Tamanho do executável/APK e bibliotecas, dependências de runtime, tempo até prompt utilizável. |
| Terminal parado | RSS/PSS conforme plataforma, CPU, wakeups e memória após estabilização. |
| Saída e interação | Throughput, latência de entrada, backlog e resposta a Ctrl-C sob saída contínua. |
| Scrollback | Memória com histórico cheio, limites e tempo de resize/rolagem. |
| Fotos e vídeo | Memória de pico e retida, CPU, FPS/tempos de quadro e descarte de imagens. |
| Telefone | Suspensão/retomada, consumo com tela ativa e em repouso, estabilidade em sessões prolongadas. |

Trate “100% Rust” como requisito de implementação a verificar com inventário de dependências: o código do produto pode ser Rust e ainda usar os serviços nativos necessários para janelas, fontes, PTY e Android. A linguagem, sozinha, não determina tamanho, memória ou bateria.

## Evidência exigida antes de concluir

Para cada plataforma, mantenha comandos de build/instalação e testes, commits, ambiente, logs e capturas que demonstrem terminal interativo, SSH, fotos, animação, vídeo e aplicações reais. Os testes devem exercer comportamento observável, incluindo encerramento e recuperação, além das unidades internas. Registre explicitamente recursos incompletos e verificações ainda não realizadas.

O objetivo completo só está atendido quando todos os requisitos têm implementação integrada e evidência correspondente. Uma entrega parcial de Linux, um núcleo compilável no Android ou uma promessa no README não concluem esse objetivo.
