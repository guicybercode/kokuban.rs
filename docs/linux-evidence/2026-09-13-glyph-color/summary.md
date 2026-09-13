# Auditoria do cache de cor de glifos em x86

Run [34744896010](https://github.com/guicybercode/kokuban.rs/actions/runs/34744896010): **PASS**. Recomendo integrar após o CI Rust de correção. Não há gate de proveniência ou pixels pendente.

Todos os oito casos com emoji ou Unicode melhoraram nos cinco pares, com faixas separadas. No ASCII sem emoji, o desenho integral de tela inteira ficou **+0,314% mais lento**, em 3/5 pares, com faixas sobrepostas. A regressão local ARM de aproximadamente 5% não se reproduziu nessa magnitude; não foi demonstrado que sua causa seja ruído.

Os valores abaixo são tempos decorridos por frame na pintura em CPU; negativo significa menos tempo.

| Conteúdo | Mudança | Pintura | Mediana antes → depois (µs) | Δ tempo | Pares mais lentos | Min–max antes / depois (µs) |
| --- | --- | --- | ---: | ---: | ---: | --- |
| ascii | single-row | integral | 945.221 → 939.024 | -0.656% | 1/5 | 940.763–948.414 / 934.736–941.913 |
| ascii | single-row | incremental | 257.362 → 249.934 | -2.886% | 0/5 | 256.465–259.244 / 248.584–254.212 |
| ascii | full-screen | integral | 942.129 → 945.085 | +0.314% | 3/5 | 939.916–948.911 / 937.119–979.898 |
| ascii | full-screen | incremental | 943.143 → 940.442 | -0.286% | 2/5 | 939.293–948.063 / 933.922–946.953 |
| ascii-after-emoji | single-row | integral | 1054.926 → 940.637 | -10.834% | 0/5 | 1049.522–1065.007 / 937.039–946.437 |
| ascii-after-emoji | single-row | incremental | 364.252 → 249.323 | -31.552% | 0/5 | 363.742–366.210 / 249.030–250.117 |
| ascii-after-emoji | full-screen | integral | 1061.959 → 944.062 | -11.102% | 0/5 | 1054.782–1070.425 / 941.834–950.582 |
| ascii-after-emoji | full-screen | incremental | 1062.530 → 940.736 | -11.463% | 0/5 | 1056.134–1069.437 / 938.735–944.077 |
| unicode | single-row | integral | 1384.263 → 1298.514 | -6.195% | 0/5 | 1372.607–1390.518 / 1293.635–1306.360 |
| unicode | single-row | incremental | 306.363 → 217.787 | -28.912% | 0/5 | 304.807–309.811 / 215.846–225.237 |
| unicode | full-screen | integral | 1391.019 → 1301.189 | -6.458% | 0/5 | 1380.027–1420.516 / 1295.427–1316.832 |
| unicode | full-screen | incremental | 1372.690 → 1301.949 | -5.154% | 0/5 | 1368.970–1384.358 / 1292.836–1318.204 |

Foram conferidos 1.167 blobs de cada tar contra o Git exato, o prefixo original de código nas duas injeções e o benchmark comum. As fontes são 24782c344714727be4f8981cd0d1d10f047aa9f4 e 965843d698edb94ac705f28c7948d26adef3edf2; harness 4a80b7e5d42fa9a4f205a5c42b06648eb7a77948. Cargo inputs e dependências conferem; os roots foram compilados com fresh=false, opt-level 3, sem debug, em targets distintos.

Os 120 resultados dos 10 logs coincidem exatamente com os records e as estatísticas do relatório. Foram comparados byte a byte 12 pares de frames e 8 equivalências ASCII antes/depois de carregar emoji. Geometria fixa 120×40 células, 1080×680 pixels, células 9×17; Unicode tem 2.800 glifos monocromáticos e 800 coloridos.

DejaVu Sans Mono 14, Noto Color Emoji e Noto CJK foram identificados e seus hashes de CI preservados. Afinidade CPU 0 observada no harness, herdada pelos filhos. Hashes de executáveis distintos foram conferidos pelo runner antes de cada processo, mas os bytes dos executáveis e das fontes tipográficas não foram fornecidos para rehash independente.

O ensaio inclui pintura na CPU e cálculo de danos no modo incremental. Exclui snapshots, rasterização inicial dos glifos, PTY e apresentação. Os modos permanecem em ordem fixa dentro de cada processo; somente a ordem das versões alterna entre pares. Não misturar este host com as medições ARM nem extrapolar estes resultados para latência de tela ou superioridade contra outros terminais.

Dados completos, faixas e deltas por par: `audit.json`. Provas de fontes/builds: `provenance.json`. Nenhum arquivo original foi alterado.
