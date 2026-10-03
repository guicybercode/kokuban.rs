# Repintura CPU com revisões e controles

O workflow manual `Linux paired CPU frame measurements` mede cálculo de dano e
pintura de uma grade de 120×40 células, com fontes reais já carregadas. Executa
seis pares, alternando três ordens AB e três BA, em Linux x86_64 e ARM64. Cada
processo aquece 30 quadros e mede 300 por cenário, preso a uma CPU permitida.

São 12 cenários: ASCII, ASCII após carregar emoji e Unicode; alteração de uma
linha ou da tela inteira; repintura completa ou incremental. Criação do snapshot,
rasterização inicial da fonte, gravação de evidências e comparação dos pixels
ficam fora do relógio. O tempo é decorrido (`Instant`), não um contador de CPU.
PTY, compositor, apresentação e teclado até a tela também ficam fora da medição.

## Comparar o aplicativo completo

```sh
gh workflow run linux-render-performance.yml \
  -f before=55c48f4 -f after=ecf90f86 -f comparison=revisions
```

Os dois aplicativos recebem exatamente o mesmo teste ignorado de repintura,
extraído da revisão do workflow. Cada lado usa uma árvore-fonte e um diretório
Cargo novos. O restante de cada aplicativo é preservado. A preparação rejeita
layouts de benchmark incompatíveis; ela não adapta silenciosamente revisões
históricas sem o teste esperado.

## Medir a variabilidade do mesmo executável

```sh
gh workflow run linux-render-performance.yml \
  -f before=55c48f4 -f after=55c48f4 -f comparison=same-binary
```

Neste modo, `after` é ignorado. Há apenas uma compilação; os dois rótulos usam
o mesmo caminho e SHA-256. A diferença observada é variabilidade da medição,
sem mudança de código. Um controle em outro runner descreve sua própria
variabilidade e não fornece um limiar universal de ruído para outro ensaio.

## Isolar o arquivo de rasterização

```sh
gh workflow run linux-render-performance.yml \
  -f before=199b987 -f after=228bc13 -f comparison=raster-only \
  -f common_source=1f2736405c9205a0ac765f98b5977f5d9a8f5581
```

Os dois lados partem de `common_source`, substituindo somente
`src/software_raster.rs`. O manifesto verifica que não existem outras diferenças
após a injeção do benchmark. Se omitido, `common_source` usa a revisão do
workflow; declare um commit exato para repetir um experimento histórico.

## Ler e auditar os resultados

O artefato de cada arquitetura dura sete dias e contém fontes originais e
preparadas, o benchmark comum, compiladores, pacotes, hashes de fontes
tipográficas, logs Cargo, executáveis, amostras brutas e 24 imagens de referência.
As imagens precisam ter dimensões válidas, estados visualmente distintos e
pixels idênticos entre os lados. SHA-256 e o checksum FNV do fixture são
recalculados sobre os bytes. Uma falha invalida o resultado da execução.

`measurements/report.json` preserva cada duração, ordem e razão pareada.
`median_paired_latency_change_percent` é a mediana das mudanças por par;
`latency_change_percent` é a razão entre as medianas dos lados. Os cálculos
podem divergir. Valores negativos indicam menos tempo. Examine também as
faixas, os pares mais lentos e a ordem AB/BA; preserve os outliers. Compare
revisões dentro do mesmo ensaio, sem atribuir diferenças entre hosts ao código.

Os resultados históricos e suas limitações estão em
[medições do raster de 13 de setembro](LINUX_RASTER_BENCHMARK_2026-09-13.md).
