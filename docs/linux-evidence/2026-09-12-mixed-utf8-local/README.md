# Triagem local de spans mistos UTF-8 — 2026-09-12

O candidato V5 aumentou a mediana de decodificação da carga Unicode em **50,42% na tela alternativa e 35,52% na principal**, com ganho nos cinco pares de cada perfil. A carga ANSI regrediu **6,54% e 3,40%**, respectivamente. São resultados locais do decoder e da grade em macOS/Apple M4; não medem a vazão de um terminal Linux completo.

## Identidade e resultados

- Fonte anterior: `469b2ac3fb88509a5ac4bf1769e4b2b3512c1008`.
- Fonte medida depois: a anterior mais [candidate.patch](candidate.patch), SHA-256 `cafbf37a84e0f768f01e576d4ec04794819bad33160ef54a4294b4b8a875bdd0`.
- Commit correspondente: [`74eaad83dbf813cb15bd883b45d52adf0dc729bd`](https://github.com/guicybercode/kokuban.rs/commit/74eaad83dbf813cb15bd883b45d52adf0dc729bd). A comparação de todos os arquivos Rust encontrou somente um comentário adicional sobre ANSI antes de `feed_byte` no commit. Os demais bytes são iguais. O patch reconstitui a fonte efetivamente medida, sem esse comentário.
- Execução: **2026-09-12 20:57:14–20:57:55 UTC**, Rust 1.94.1 Homebrew, LLVM 21.1.8, `aarch64-apple-darwin`. O JSON original registra Darwin 25.2.0/arm64; a identificação Apple M4 vem do ambiente informado na sessão. A versão macOS 26.2 (25C56) foi consultada na preparação deste pacote.

MiB/s abaixo é a mediana de cinco medianas de processo. Cada mediana de processo contém oito rounds. Delta = `(mediana depois / mediana antes − 1) × 100`; não é a mediana dos deltas dos pares.

| Tela | Carga | Antes MiB/s | Depois MiB/s | Delta | Pares mais lentos |
| --- | --- | ---: | ---: | ---: | ---: |
| Alternativa | ASCII | 383,341 | 379,347 | −1,04% | 4/5 |
| Alternativa | ANSI | 256,077 | 239,320 | −6,54% | 4/5 |
| Alternativa | Unicode | 93,523 | 140,681 | +50,42% | 0/5 |
| Alternativa | Linhas curtas | 84,283 | 86,150 | +2,22% | 1/5 |
| Principal | ASCII | 210,168 | 208,332 | −0,87% | 3/5 |
| Principal | ANSI | 155,243 | 149,960 | −3,40% | 3/5 |
| Principal | Unicode | 74,185 | 100,538 | +35,52% | 0/5 |
| Principal | Linhas curtas | 16,115 | 16,908 | +4,92% | 2/5 |

As faixas das medianas dos processos Unicode não se sobrepõem: **87,348–100,115 → 130,914–144,792 MiB/s** na alternativa e **71,307–79,879 → 94,959–104,008** na principal. As demais faixas se sobrepõem; as perdas observadas permanecem registradas. [summary.json](summary.json) preserva precisão completa, faixas e deltas de cada par. [screening.json](measurements/screening.json) contém os 80 registros de processo e todos os 640 rounds; os rounds de um mesmo processo compartilham estado e não são 640 repetições independentes.

## Método e limites

O [helper Rust](harness-candidate/main.rs) importa os módulos reais de gráficos, grade, parser e decoder. Ele cria uma grade **80×24, histórico de 10.000 linhas**, desabilita Kitty/Sixel, carrega o payload antes de cronometrar e faz uma decodificação completa de aquecimento. Depois mede oito decodificações no mesmo estado, em chunks de **16 KiB**. A tela alternativa é selecionada antes do aquecimento; a principal mantém o histórico. O intervalo inclui `feed_until_event` e a mutação da grade, com `black_box`, e exclui leitura do arquivo e inicialização do processo.

Cada combinação de perfil, par, lado e carga usa um processo novo: **cinco pares alternados AB/BA por perfil, quatro cargas e dois lados**, primeiro todos os pares da tela alternativa e depois os da principal. Cada payload contém aproximadamente 4 MiB de linhas inteiras; não há truncamento de uma linha. As linhas vêm da função `payloads(4 * 1024 * 1024)` de [compare-terminal-performance.py](reference/compare-terminal-performance.py), na revisão `8dafe9e5ba4106994cae27d702778a277f108199`. [generate-payloads.py](generate-payloads.py) extrai essa função da cópia preservada e verifica tamanho e SHA-256 antes de gravar os quatro arquivos. Os bytes regenerados foram comparados com os inputs usados na medição.

Os dois helpers têm `Cargo.toml` e `Cargo.lock` idênticos, release com `debug=1`, `incremental=false` e otimização padrão de release. Eles são pacotes mínimos separados do produto completo: não incluem janela, PTY, threads de leitura, compositor, atlas ou renderizador. O conjunto menor de módulos e a organização do binário podem mudar decisões do compilador. O lock do helper resolve versões diferentes de `proc-macro2`, `quote`, `syn` e `unicode-ident` em relação ao lock do produto; essas versões são iguais entre os dois lados. Ambos os manifests/locks do produto e dos helpers estão retidos para inspeção. Este teste não substitui o build release completo nem mede apresentação, input-to-photon, memória ou consumo de CPU.

Não houve builds ou geração de benchmark pertencentes a esta medição durante os rounds. **O host não estava isolado de outras sessões/processos**, não houve afinidade de CPU ou controle térmico e não foi registrada a frequência dos núcleos. As médias de carga antes/depois permanecem no JSON. O desenho alternado reduz alguns efeitos de ordem, mas não elimina interferência. Não se deve transportar estes deltas para Linux, comparar diretamente estes MiB/s com ensaios via PTY, nem usá-los para classificar Ghostty, Alacritty ou Kitty.

## Compilação, assembly e validação

O executável anterior foi congelado a partir de `/tmp/kokuban-mixed-spans-prototype/baseline-target/release/kokuban-core-profile`. O V5 foi compilado em `/tmp/kokuban-mixed-forced-prototype/candidate-target`, sem compartilhar o diretório target do anterior. Os [logs anterior](logs/baseline-build.log) e [V5](logs/candidate-build.log) registram a compilação do pacote raiz nos diretórios de fonte correspondentes. As cópias medidas foram conferidas byte a byte com esses executáveis:

| Executável | SHA-256 |
| --- | --- |
| Antes | `034bfee991d09c99a0e805b442516233d59dcd642b26fd6f1406c5e4afdf1481` |
| V5 | `d15335821d218df907d9a724d53e4169e4066b5e358589f22f2560b3b3bb6b72` |

[measure.py](measure.py) rejeita executáveis com hashes iguais antes de iniciar. Este pacote contém apenas a execução V5 válida. Uma tentativa anterior com binários iguais pertence a outro protótipo e não fornece nenhuma amostra deste conjunto. [candidate-audit.original.json](candidate-audit.original.json) é o registro original criado antes da análise de assembly e da execução: sua indicação “pending” está desatualizada. [source-provenance.json](source-provenance.json) registra a auditoria posterior e os hashes de todos os 60 arquivos Rust anteriores e 61 posteriores.

O V5 mantém `feed_utf8_text` com `#[inline(never)]` e `feed_byte` com `#[inline(always)]`. A inspeção do executável medido não encontrou símbolo ou chamada para `feed_byte`; o caminho de bytes chama `Parser::advance` diretamente em `0x10000a1b8`, e o helper UTF-8 permanece separado em `0x100009cb8`. O [extrato de assembly](logs/v5-assembly-excerpt.txt), a listagem de símbolos e os hashes dos dumps estão preservados. Mesmo com essa organização, **as regressões ANSI continuam presentes na medição V5**. A inspeção estática não quantifica sua causa total; este pacote não contém uma comparação de assembly com V2.

O helper V5 passou [241 testes locais](logs/helper-tests.log). `check --all-targets` passou localmente conforme registrado na sessão, e o [Clippy local](logs/local-clippy.log) terminou com sucesso e warnings. O teste completo local não chegou ao fim: a compilação falhou por falta de espaço (`ENOSPC`). A validação completa foi concluída no [CI 34719026097](https://github.com/guicybercode/kokuban.rs/actions/runs/34719026097), na revisão `74eaad83`: **845 testes Rust macOS (604 + 241)** e **944 Linux (703 + 241, um ignorado)**, além de check, Clippy e verificação das tabelas Unicode. [ci-excerpt.txt](logs/ci-excerpt.txt) preserva as linhas de checkout, comandos e resultados, com números de linha do log original e seu hash em `source-provenance.json`. Esses testes validam comportamento; não transformam a triagem local em resultado de desempenho Linux.

## Auditar e repetir

Na raiz do repositório, a auditoria abaixo confere o inventário, ordem AB/BA, todos os rounds, estatísticas e geração dos payloads em memória, sem compilar ou medir:

```sh
python3 -B docs/linux-evidence/2026-09-12-mixed-utf8-local/audit.py
```

Para reconstruir **a fonte medida** em um diretório novo, com os commits disponíveis localmente:

```sh
evidence="$PWD/docs/linux-evidence/2026-09-12-mixed-utf8-local"
scratch="$(mktemp -d /tmp/kokuban-mixed-repro.XXXXXX)"
mkdir "$scratch/baseline" "$scratch/candidate"
git archive 469b2ac3fb88509a5ac4bf1769e4b2b3512c1008 | tar -x -C "$scratch/baseline"
cp -R "$scratch/baseline/." "$scratch/candidate/"
patch -p1 -d "$scratch/candidate" < "$evidence/candidate.patch"
cp -R "$evidence/harness-baseline" "$evidence/harness-candidate" "$scratch/"
cp "$evidence/measure.py" "$evidence/baseline-revision.txt" "$evidence/candidate.patch" "$scratch/"
python3 -B "$evidence/audit.py" --source-root "$scratch"
python3 -B "$evidence/generate-payloads.py" "$scratch/measurements"
```

Use Rust 1.94.1 e dependências do lock disponíveis no cache para `--offline`. Compile cada pacote raiz em seu próprio target, conferindo os logs e a diferença entre hashes antes de medir. Os caminhos de build diferentes podem alterar os hashes devido às informações de debug; não se exige reproduzir byte a byte os executáveis originais.

```sh
CARGO_TARGET_DIR="$scratch/baseline-target" cargo build \
  --manifest-path "$scratch/harness-baseline/Cargo.toml" --release --locked --offline \
  > "$scratch/baseline-build.log" 2>&1
CARGO_TARGET_DIR="$scratch/candidate-target" cargo build \
  --manifest-path "$scratch/harness-candidate/Cargo.toml" --release --locked --offline \
  > "$scratch/candidate-build.log" 2>&1
cp "$scratch/baseline-target/release/kokuban-core-profile" "$scratch/measurements/before"
cp "$scratch/candidate-target/release/kokuban-core-profile" "$scratch/measurements/after"
shasum -a 256 "$scratch/measurements/before" "$scratch/measurements/after"
python3 -B "$scratch/measure.py"
```

Os logs de build devem mostrar `Compiling kokuban-core-profile` com a fonte correta; não reutilize um único target entre os helpers. A nova execução grava apenas em `$scratch/measurements/screening.json`. Evite builds e outros benchmarks simultâneos e documente o estado do host; outra máquina ou condição não precisa reproduzir os números desta amostra.

[inventory.json](inventory.json) cobre todos os arquivos do pacote, exceto ele próprio. Executáveis, payloads binários e diretórios target não foram copiados para o repositório.
