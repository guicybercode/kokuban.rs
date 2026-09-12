# Fronteiras Unicode por páginas — Linux, 11/09/2026

Comparação de `34ab430682a81ec0a91ee3bae740daeb91b2fefc` para
`8dafe9e5ba4106994cae27d702778a277f108199`, com executor fixado na segunda
revisão. Unicode melhorou nos cinco pares de ambos os perfis: **+41,26% na
tela alternativa e +35,63% na principal**, pela razão entre medianas.
ASCII e linhas curtas perderam, respectivamente, 0,53% e 0,23% na principal.
As tabelas estão em [Medições Linux](../../LINUX_PERFORMANCE.md#fronteiras-unicode-por-páginas-dois-perfis-em-2026-09-11).

- [Run alternativo 34626144701](https://github.com/guicybercode/kokuban.rs/actions/runs/34626144701):
  AMD EPYC 7763, sem histórico; [relatório bruto](ci-alternate-report.json).
- [Run principal 34626168671](https://github.com/guicybercode/kokuban.rs/actions/runs/34626168671):
  AMD EPYC 9V74, histórico de 10.000 linhas; [relatório bruto](ci-primary-report.json).

Compare as revisões dentro de cada run. Os runners diferentes impedem
atribuir a diferença entre perfis somente ao histórico. Cada run concluiu
cinco pares AB/BA/AB/BA/AB, 10 processos, 40 cargas de aproximadamente
32 MiB e 300 RTTs. Ubuntu 24.04 x86_64, Rust 1.94.1 release, Weston 13
headless/Pixman, DejaVu Sans Mono 14, grade 80×24 e 720×408 pixels.
O executor registrou afinidade `[0]`; os filhos a herdam, sem leitura
individual da afinidade de cada terminal.

O [manifesto](manifest.json) preserva todas as razões por par, intervalos,
MAD e contagens de perdas. Na alternativa, ASCII teve dois pares piores,
incluindo −8,30% no quinto. Na principal, ASCII piorou em quatro pares e
linhas curtas em três. Só Unicode apresentou faixas mínimo–máximo sem
sobreposição. Cinco pares não estabelecem significância estatística ou
ganho universal.

A auditoria baixou os ZIPs oficiais, conferiu seus SHA-256 contra os digests
do GitHub e verificou todos os 87 arquivos de cada artefato contra os
downloads locais. Os inventários [alternativo](alternate/artifact-inventory.json)
e [principal](primary/artifact-inventory.json) registram tamanhos e hashes.
Relatórios brutos, configurações, 20 logs de terminal, ambiente, metadados
dos runs, hashes dos executáveis e trechos numerados dos logs foram retidos.
Estatísticas foram recalculadas das amostras; `case.json`, `sample.json` e
`result.json` foram conferidos, assim como geometria, resposta DSR final,
histórico e payloads regenerados pelo código fixado.

Os arquivos Cargo do artefato coincidem com o Git de cada revisão. O
workflow e os logs confirmam fontes extraídos com `git archive` e targets
separados. Arquivos Cargo, árvores Cargo e scripts já rastreados iguais
são referenciados por caminho e SHA-256; scripts também têm revisão e blob
Git fixados. O campo bruto `source_refs_verified_by_runner: false` permanece
intacto: o executor não verifica os rótulos, e a auditoria externa os
conferiu contra os builds do workflow.

Hashes de executáveis são registros do CI: os bytes não foram retidos
para rehash independente. Esses workflows validam o executor Python e
compilam as duas revisões, sem executar testes Rust. O ensaio mede
processamento PTY/DSR; não comprova equivalência de texto ou pixels,
latência visual, desempenho contra concorrentes ou uso físico em
Omarchy/Hyprland com GPU e monitor.
