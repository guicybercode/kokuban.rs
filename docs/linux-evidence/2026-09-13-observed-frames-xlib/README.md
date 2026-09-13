# Resposta visual com Xlib persistente em 13 de setembro

O [run 34747011215](https://github.com/guicybercode/kokuban.rs/actions/runs/34747011215)
mediu 120 eventos por terminal usando uma conexão Xlib persistente. O intervalo
inclui XTest, o aplicativo controlado no PTY, pintura, agendamento e leitura
X11 até a cópia dos pixels. É um limite superior observado em Xvfb; apresentação
física e GPU não são medidas.

| Terminal | Mediana (ms) | p95 (ms) | p99 (ms) |
| --- | ---: | ---: | ---: |
| Kokuban `55c48f4` | 2,313 | 2,480 | 2,521 |
| Alacritty 0.16.1 | 14,313 | 15,564 | 16,112 |
| Ghostty 1.3.0-dev+0000000 | 14,323 | 15,875 | 16,437 |
| Kitty 0.45.0 | 15,182 | 16,327 | 16,974 |

O host foi AMD EPYC 7763, com Ubuntu 26.04 fixado por digest, Xvfb persistente
e Mesa llvmpipe. Foram três processos por terminal, 40 eventos medidos e cinco
aquecimentos por processo, em ordem rotacionada. Todos mantiveram 80×24 células,
9×17 pixels por célula e janelas 720×408. O relatório preserva as versões dos
pacotes; o Ghostty é o build de desenvolvimento identificado pelo Ubuntu.

O intervalo entre capturas malsucedidas foi 1 ms. No Kokuban, todos os 120
eventos precisaram de duas capturas. As medianas foram 0,016 ms para injeção
e 0,581 ms por captura. A pausa de polling e as validações anteriores também
entram no limite superior; a validação final ocorre depois do timestamp.
Esses componentes não devem ser subtraídos como se fossem independentes.

A auditoria conferiu 540 eventos incluindo aquecimento, 3.393 capturas
registradas e 9.451.728 pixels nas 36 imagens retidas. Também verificou oito
preflights de calibração, sequência das teclas, dimensões, cores e timestamps.
A suíte de 26 testes executou o probe C da ABI contra os headers Linux, sem
testes omitidos. A ausência de subprocessos no intervalo Xlib foi conferida
por fonte e teste de integração; não foi registrado um trace de syscalls.

O [relatório bruto](report.json), a
[auditoria](independent-frame-latency-audit.json), os logs de ABI e a proveniência
selecionada ficam preservados com hashes no [manifesto](manifest.json).
`selected-snapshots.tar.gz` contém todas as imagens inicial, primeira transição
e final, sem perda. Capturas intermediárias têm registros e hashes; seus
bitmaps e os bytes dos executáveis, bibliotecas e fontes não foram retidos
pelo workflow. O artefato completo expira após sete dias.

Os percentis usam nearest rank; eventos do mesmo processo são correlacionados
e os percentis por processo também estão na auditoria. Este ensaio cobre fundos
opacos. A [medição com subprocessos xwd](../2026-09-13-observed-frames-xwd/README.md)
usou o mesmo código Kokuban em outro host e com outro observador: a diferença
entre os números não quantifica aceleração do aplicativo. Veja o
[método e reprodução](../../LINUX_FRAME_LATENCY.md).
