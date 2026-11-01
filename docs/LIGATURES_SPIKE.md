# Spike: ligaduras por shaping de runs

Pergunta do spike: o Kokuban consegue desenhar as ligaduras de fontes de
programação (Fira Code, JetBrains Mono, Cascadia) sem prejudicar a vazão?
Resultado: protótipo funcional atrás de `font.ligatures = true` (padrão
`false`), medido, com recomendação **GO condicional** ao fim deste documento.

Branch: `spike/ligaduras`. Tempo gasto: cerca de 2 horas.

## Por que hoje não há ligaduras

O atlas molda cada célula sozinha: `ShapingFont::shape` em
`src/glyph_atlas/text_raster.rs` recebe um grafema por vez e chama
`rustybuzz::shape` sem features. O `calt` da fonte nunca vê `-` e `>` juntos,
então nada liga. Não é limitação do rustybuzz nem da fonte: é a unidade de
shaping.

## O que foi feito

1. **`src/glyph_atlas/runs.rs` (novo).** Corta a linha em runs de células ASCII
   imprimíveis (`!`..`~`, espaço quebra) com o mesmo estilo bold/itálico, molda
   cada run uma vez com `calt=1`, `liga=1`, `dlig=0` e devolve só o que difere
   do desenho por célula, como `RunGlyph { start_col, span, glyph_id, style,
   x_offset }`. Dois esquemas de fonte são cobertos:
   - cluster largo (um glifo cobre N colunas, advance = N células);
   - espaçador + glifo largo (estilo Fira Code: cada coluna mantém o próprio
     cluster, o último glifo desenha "para trás" com bearing negativo). Foi o
     que a JetBrainsMono Nerd Font Mono instalada produziu: `->` vira o glifo
     12137 (vazio) na primeira coluna e o glifo 11919 (18 px de largura,
     bearing −9) na segunda.
   Runs com advance fora da grade de células, cluster com mais de um glifo ou
   `.notdef` são descartados inteiros e voltam ao caminho por célula. Runs só
   alfanuméricos (fora `ww`) nem entram no shaper.
2. **Cache.** Linha moldada cacheada por hash de (escalares, estilos), 512
   entradas, geração nova quando enche (uma tela cabe inteira). Glifos de
   ligadura cacheados no atlas por `(glyph_id, estilo, x_offset)` e
   rasterizados por id via `rasterize_outline` (`TextRasterizer::rasterize_glyph`).
   Tudo é limpo em `clear_and_resize` e quando a opção muda.
3. **Gancho no Metal** (`build_pane_vertices`, `src/renderer/metal.rs`): antes
   do laço de colunas a linha pede `atlas.row_run_glyphs(...)`; na coluna
   inicial de um `RunGlyph` o quad usa `get_or_insert_run_glyph`, e as colunas
   cobertas (`span > 1`) só desenham o fundo. Com a opção desligada o laço é
   byte a byte o de antes (`no_run_glyphs()` devolve uma lista vazia e cada
   coluna cai em `get_or_insert_cell`).
4. **Config.** `FontConfig::ligatures: bool` (padrão `false`), aplicado em
   `app/mod.rs` e preservado quando `viewDidChangeBackingProperties` recria o
   atlas numa troca de escala (sem isso a opção se perdia ao mover a janela
   para o monitor externo).
5. **Testes** (`cargo test glyph_atlas::runs` e `ligature_tests`):
   - Menlo (sem GSUB de ligaduras) produz lista vazia para uma linha cheia de
     `->`, `!=`, `<=`, `www`: prova que o caminho de runs não altera o desenho
     de fontes sem ligaduras.
   - JetBrainsMono Nerd Font Mono produz glifos de run cobrindo `->` e `!=`
     com ids diferentes dos nominais.
   - Linhas idênticas devolvem o mesmo `Arc` (cache de linha).
   - Os glifos de run rasterizam em entradas de até 3 células de largura no
     atlas real (`run_glyphs_rasterize_into_cell_sized_entries`).

## Fontes disponíveis nesta máquina

`fc-list` só encontrou a família JetBrains Mono (Nerd Font, variantes NF/NFM/
NFP e as NL = sem ligaduras). Fira Code e Cascadia não estão instaladas e
instalar não era permitido. O protótipo foi exercitado com
`JetBrainsMono Nerd Font Mono`; o esquema de cluster largo (Cascadia) está
coberto no código, mas não foi exercitado com fonte real.

## Custo de shaping

`cargo test --release shape_runs_cost -- --ignored --nocapture`, Apple M4,
10 000 linhas de 80 colunas, texto denso em operadores
(`fn main() -> Result<(), E> { if a != b && c >= d || ... }`):

| Cenário (ns por linha de 80 colunas) | JetBrainsMono NFM | Menlo |
|---|---:|---:|
| Primeira versão, `Face` montada por run | 344 344 | 22 566 |
| Atual: `Face` por estilo e por linha, runs só alfanuméricos pulados | **81 595** | 18 449 |
| Idem com `Face` compartilhada (limite se a Face for cacheada) | 70 770 | 17 905 |
| Linha quente (cache de linha) | **268** | 261 |
| Referência: shaping por célula de hoje em falta de cache (80 shapes) | 241 891 | 57 099 |

Leitura:

- Frame estável (texto não mudou): 24 linhas × 0,27 µs ≈ 6 µs. Irrelevante.
- Pior caso de scroll (toda linha nova a cada frame, texto denso em
  operadores): 24 linhas ≈ 2 ms, 60 linhas ≈ 5 ms, dentro dos 16 ms de um
  frame a 60 Hz. Texto comum (prosa, logs) é quase todo alfanumérico e nem
  chega ao shaper.
- O decodificador não é tocado: o shaping acontece só na thread de desenho e
  só em falta no cache de linha. A vazão medida por `terminal-throughput` não
  muda por construção.
- `cargo test` completo: 718 passaram, 2 ignorados, 1,7 s de testes (5 s com
  build incremental).

## O que desenha hoje

- Com `ligatures = true` e uma fonte com `calt`, o atlas recebe os glifos de
  ligadura e o laço do Metal emite o quad largo na coluna inicial (validado
  por teste unitário no atlas real: `->`, `=>`, `<=`, `!=`, `===`, `<!--`,
  `-->` etc. geram entradas de 10 a 37 px para célula de 10 px).
- Não há captura de tela anexada: `screencapture -l <id>` devolveu imagem
  branca numa instância e "could not create image from window" nas seguintes
  neste setup de dois monitores; a instância de referência sem ligaduras
  capturou normalmente. A evidência visual fica para a etapa de GO.

## Ressalvas de correção

- **Cursor dentro de uma ligadura:** o bloco do cursor (alfa 180) cobre só a
  célula do cursor; o glifo largo continua inteiro por baixo. Aceitável para
  o spike; outros terminais fazem o mesmo.
- **Seleção/busca dentro de uma ligadura:** o fundo é por célula (correto),
  mas o quad do glifo leva o `bg` da coluna inicial e o shader faz
  `mix(bg, fg, coverage)`, então a caixa de tinta do glifo sobre a célula
  selecionada mostra o fundo não selecionado atrás da tinta. Corrigir
  dividindo o quad por célula (sub-retângulos de UV) ou passando o fundo real
  de cada célula.
- **Glifos com bearing negativo** (esquema Fira) desenham sobre a célula
  anterior com o `bg` da própria célula; mesmo efeito quando as duas células
  têm fundos diferentes.
- **Caracteres largos, grafemas compostos e células ocultas** quebram runs e
  seguem pelo caminho atual; nunca participam de uma ligadura.
- **Teclado kitty, parser, grid:** intocados; a mudança é só de desenho.
- **Linux (`software_raster`)**: sem gancho; a opção não tem efeito lá.
- **Cache de linha por hash u64**: colisão trocaria as ligaduras de uma linha;
  improvável com 512 entradas, mas guardar a chave completa é barato.
- **Janela branca** vista na primeira instância de teste (processo vivo,
  frames sendo construídos): não se repetiu em duas relançadas e não há
  pânico no log. Fica como item a observar na etapa de GO.

## Recomendação: GO (condicional)

O caminho é viável e contido: ~400 linhas novas em `glyph_atlas/`, ~40 no
laço do Metal, flag desligada por padrão, nenhuma mudança em `Cell`, parser,
grid ou decodificador. O custo em frame estável é desprezível e o pior caso
de scroll cabe no frame.

Trabalho estimado para fechar (2 a 3 dias):

1. Evidência visual e QA manual com JetBrains Mono, Fira Code e Cascadia
   (cluster largo real) — 0,5 dia.
2. Seleção/busca dentro de ligadura: quad por célula com UV parcial — 0,5 dia.
3. Cachear a `rustybuzz::Face` por estilo (dados `'static` ou self-ref
   seguro) e trocar o cache de linha por cache de run (`fn`, `->`, `!=`
   repetem muito) — 0,5 dia; leva a linha fria de ~82 µs para bem abaixo de
   ~70 µs.
4. Guardar a chave completa no cache de linha; invalidar ao trocar fonte.
5. Gancho equivalente no `software_raster` do Linux — 0,5 dia.
6. Documentar `font.ligatures` em `docs/INSTALL.md` e no `kokuban.toml`.
7. Investigar a janela branca da primeira instância antes de ligar por padrão.

Se a prioridade do Linux for alta, o item 5 pode entrar junto; o núcleo
(`runs.rs` + atlas) já é independente de renderizador.
