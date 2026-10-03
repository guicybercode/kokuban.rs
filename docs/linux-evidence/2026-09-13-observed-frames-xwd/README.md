# Resposta visual X11 observada em 13 de setembro

O [run 34745820606](https://github.com/guicybercode/kokuban.rs/actions/runs/34745820606)
verificou alterações de um retângulo opaco nos quatro terminais. Estes tempos
são limites superiores desde a injeção sintética até a captura dos pixels;
incluem o custo de `xdotool` e `xwd`. Não medem teclado ou apresentação físicos.

| Terminal | Mediana (ms) | p95 (ms) | p99 (ms) |
| --- | ---: | ---: | ---: |
| Kokuban `55c48f4` | 5,329 | 5,635 | 5,681 |
| Alacritty 0.16.1 | 11,163 | 15,432 | 15,546 |
| Ghostty 1.3.0-dev+0000000 | 12,722 | 26,709 | 29,573 |
| Kitty 0.45.0 | 15,580 | 16,273 | 17,993 |

Foram 120 eventos medidos por terminal, em três processos novos e ordem
rotacionada, mais cinco aquecimentos por processo. Os percentis usam nearest
rank. Eventos do mesmo processo são correlacionados; a auditoria também retém
os percentis por processo. O Ghostty é o build de desenvolvimento identificado
pelo pacote Ubuntu `1.3.0~us1-0ubuntu1.1`, não uma versão estável presumida.

O ambiente foi AMD EPYC 9V74, Ubuntu 26.04 fixado por digest, Xvfb persistente
e Mesa llvmpipe, com a afinidade completa do runner. Todos os processos
mantiveram 80×24 células, 9×17 pixels por célula e janelas 720×408. O espaço
extra inicial do Kitty foi ajustado antes do aquecimento. Pacotes, fontes,
binários e configurações estão identificados no relatório bruto.

No Kokuban, a mediana de injeção foi 2,296 ms e a da captura, 2,997 ms; os 120
eventos já tinham o quadro correto na primeira captura. O custo do observador
limita a resolução deste ensaio. Esses componentes não são independentes e
não devem ser subtraídos para estimar uma latência interna.

A auditoria conferiu oito preflights de calibração, 540 eventos incluindo
aquecimento, 1.228 capturas registradas e 9.451.728 pixels nas 36 imagens
retidas. A alternância completa das cores, geometria e sequência de teclas
precisou passar; uma resposta DSR sozinha não satisfaz o teste.

Este pacote preserva o [relatório bruto](report.json), a
[auditoria](independent-frame-latency-audit.json), a proveniência selecionada e
todas as imagens inicial, primeira transição e final, compactadas sem perda em
`selected-snapshots.tar.gz`. O [manifesto](manifest.json) permite conferir seus
hashes. As capturas intermediárias têm registros e hashes, mas não arquivos
bitmap; executáveis e bytes das fontes também não foram retidos pelo workflow.
O artefato completo do run expira após sete dias.

O resultado descreve atualização de fundos opacos nessa sessão virtual. Ele
não estabelece uma classificação geral de terminais, fidelidade de texto ou
latência em Omarchy com GPU e monitor. O smoke completo anterior
[`34745354067`](https://github.com/guicybercode/kokuban.rs/actions/runs/34745354067)
usou AMD EPYC 9V45; diferenças absolutas entre esses hosts não isolam efeito
do código. Veja o [método e reprodução](../../LINUX_FRAME_LATENCY.md).
