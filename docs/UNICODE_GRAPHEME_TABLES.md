# Tabelas de fronteiras de grafemas

O grid usa um caminho rápido conservador antes do `GraphemeCursor`, baseado
nas categorias de `unicode-segmentation 1.13.3`, com Unicode 17.0.0. A decisão
depende apenas do último escalar do grafema existente e da categoria
certificada do próximo escalar:

| Categoria do sucessor na crate | O escalar estende o grafema quando |
| --- | --- |
| `GC_Any` | O anterior é `GC_Prepend` |
| `GC_Extend`, `GC_ZWJ` ou `GC_SpacingMark` | O anterior não é Control, CR ou LF |
| Categoria desconhecida ou página mista | O `GraphemeCursor` existente decide |

Essas decisões seguem a precedência de GB4, GB9, GB9a, GB9b e GB999 nas
[regras de fronteiras da UAX #29, Unicode 17](https://www.unicode.org/reports/tr29/tr29-47.html#Grapheme_Cluster_Boundary_Rules).
`GC_Any` é a categoria interna da crate: ela exclui `Extended_Pictographic` e
`InCB_Consonant`, mesmo quando esses caracteres têm `Grapheme_Cluster_Break=Other`.
Usar apenas essa propriedade ou uma classificação de letras seria incorreto.
Os casos contextuais de RI, Indic, Hangul e emoji continuam no fallback.

A tabela de produção certifica somente páginas uniformes de 64 pontos de
código. Ela ocupa 4.352 bytes, com dois bits por página, além dos predicados
de Prepend e controle. ASCII imprimível tem classificação direta. O caminho
não mantém cache por célula, estado entre chamadas ou dados específicos de
workload, e não altera a construção ou a largura dos grafemas.

## Regeneração

O gerador requer Python 3.11 ou posterior. Ele encontra a raiz do checkout
pelo próprio arquivo, lê o `Cargo.lock` real e procura o arquivo `.crate` em
`$CARGO_HOME/registry/cache/`, usando `~/.cargo` quando `CARGO_HOME` não estiver
definido. Não baixa arquivos nem executa Cargo automaticamente.

Na raiz do checkout:

```sh
cargo fetch --locked
python3 scripts/generate-grapheme-tables.py
python3 scripts/generate-grapheme-tables.py --check
```

Para usar um arquivo já disponível fora do cache:

```sh
python3 scripts/generate-grapheme-tables.py --check \
  --crate-path /caminho/unicode-segmentation-1.13.3.crate
```

O modo normal escreve somente arquivos cujos bytes mudaram. `--check`
regenera os resultados em memória, audita o domínio e exige igualdade dos
arquivos existentes, sem escrevê-los. Um cache ausente pede `cargo fetch
--locked` ou `--crate-path`. Execute sem `-O` e sem `PYTHONOPTIMIZE`: o gerador
rejeita esse modo para garantir que suas verificações não sejam desativadas.

## Proveniência e validação

O [manifesto gerado](../src/grid/boundary_pages_manifest.json) registra versão,
checksum da crate, hashes dos fontes e do gerador, arquivos produzidos,
contagens de cobertura e licença. Não contém caminhos da máquina, horários
ou referências a harnesses de medição.

Antes de extrair qualquer dado, o gerador confere o SHA-256 do arquivo `.crate`
contra o lock e o checksum auditado. A versão e o hash de `src/tables.rs`
também são fixados. A análise da tabela rejeita conteúdo não reconhecido,
categorias desconhecidas, intervalos sobrepostos ou quantidade inesperada de
intervalos; a extração não executa código da crate.

A auditoria visita todos os 1.112.064 escalares Unicode válidos. A tabela
compacta é comparada com uma busca independente nos 1.618 intervalos
originais, incluindo os predicados completos de Prepend e Control/CR/LF.
Pontos substitutos UTF-16 não são escalares Rust. As contagens incluem pontos
não atribuídos e não representam cobertura de linguagens ou de desempenho.

Os testes Rust mantêm dois oráculos separados da classificação compacta:

- [Intervalos originais](../src/grid/boundary_pages_reference.rs), consultados
  por busca binária para verificar todo o domínio classificado.
- [Corpus oficial Unicode 17](../src/grid/boundary_pages_corpus.rs), extraído
  sem recalcular fronteiras de `TEST_SAME` e `TEST_DIFF` da crate: 766 casos
  reconstruídos escalar por escalar.

Os testes adicionais comparam extremos de propriedades e contextos longos
com o segmentador completo, incluindo Prepend seguido de controle, marcas
combinantes, paridade de RI, Indic, Hangul e emoji ZWJ. Os testes existentes
de margem, resize, reflow e PTY fragmentado continuam usando o mesmo grid.
Os oráculos e o corpus são compilados apenas nos testes.

## Atualização e licença

O `Cargo.toml` fixa `unicode-segmentation = "=1.13.3"`. Isso impede mudanças
silenciosas de categoria em uma versão posterior da crate que ainda declare
Unicode 17. A verificação de `unicode_segmentation::UNICODE_VERSION` fornece
uma guarda adicional: divergência desativa o caminho rápido.

Uma atualização exige revisar as regras e categorias, atualizar em conjunto
o pin, o lock e as constantes auditadas do gerador, regenerar os dados e
examinar o diff. Depois, execute `--check`, os testes completos e uma medição
pareada antes de atribuir ganho ao novo caminho. O gerador recusa uma versão
ou checksum novo até essa revisão explícita.

Os arquivos derivados conservam a atribuição original e usam a opção MIT da
crate. O texto integral da [licença MIT](../THIRD_PARTY_LICENSES/unicode-segmentation/LICENSE-MIT)
e o [aviso original](../THIRD_PARTY_LICENSES/unicode-segmentation/COPYRIGHT)
permanecem no repositório e são conferidos pelo gerador.
