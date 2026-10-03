# Blocos mistos de UTF-8 e ASCII — Linux, 12/09/2026

A carga Unicode ganhou **18,44% na tela alternativa e 17,60% na primária**, pela razão entre medianas, melhorando nos dez pares. As demais cargas melhoraram nos cinco pares alternativos. Na primária, ASCII teve dois pares mais lentos e ANSI apresentou uma pequena perda entre medianas. Os resultados descrevem processamento neste ambiente; não demonstram superioridade visual ou desempenho geral contra outros terminais.

Comparamos `469b2ac3fb88509a5ac4bf1769e4b2b3512c1008` com `74eaad83dbf813cb15bd883b45d52adf0dc729bd`, usando o executor da segunda revisão. O parser processa blocos contíguos de texto UTF-8 misturado com ASCII imprimível, e a grade agrupa escritas compatíveis. Controles, eventos, recuperação de UTF-8 inválido e casos de grafemas que exigem contexto mantêm seus caminhos de tratamento. Alterações e merges posteriores da branch não fazem parte das revisões medidas neste pacote.

## Medianas e pares

[Tela alternativa, run 34721333020](https://github.com/guicybercode/kokuban.rs/actions/runs/34721333020), sem histórico:

| Carga | Antes, MiB/s | Depois, MiB/s | Variação entre medianas | Mediana das variações por par | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 75,023 | 79,956 | +6,58% | +7,24% | 0/5 |
| ANSI | 65,094 | 68,926 | +5,89% | +5,44% | 0/5 |
| Unicode | 33,540 | 39,726 | +18,44% | +17,82% | 0/5 |
| Linhas curtas | 30,933 | 34,258 | +10,75% | +10,38% | 0/5 |

[Tela primária, run 34721365964](https://github.com/guicybercode/kokuban.rs/actions/runs/34721365964), com histórico de 10.000 linhas:

| Carga | Antes, MiB/s | Depois, MiB/s | Variação entre medianas | Mediana das variações por par | Pares mais lentos depois |
| --- | ---: | ---: | ---: | ---: | ---: |
| ASCII | 54,206 | 55,740 | +2,83% | +1,89% | 2/5 |
| ANSI | 47,774448 | 47,767397 | −0,01476% | +1,05297% | 1/5 |
| Unicode | 26,979 | 31,727 | +17,60% | +17,60% | 0/5 |
| Linhas curtas | 7,319 | 7,631 | +4,26% | +5,26% | 0/5 |

A razão entre medianas e a mediana das razões por par são cálculos distintos. ANSI primário caiu ligeiramente pela primeira agregação, embora quatro pares tenham melhorado e a mediana pareada seja positiva. Ambos os resultados permanecem registrados; a perda não foi arredondada para zero.

Faixas mínimo–máximo, em MiB/s:

| Perfil | Carga | Antes | Depois | Sobreposição |
| --- | --- | ---: | ---: | --- |
| Alternativo | ASCII | 72,601–76,869 | 78,133–81,140 | Não |
| Alternativo | ANSI | 61,892–67,981 | 65,261–69,296 | Sim |
| Alternativo | Unicode | 32,701–33,717 | 38,889–40,518 | Não |
| Alternativo | Linhas curtas | 29,959–31,372 | 33,833–34,368 | Não |
| Primário | ASCII | 52,589–55,281 | 53,635–56,675 | Sim |
| Primário | ANSI | 45,278–48,387 | 45,561–49,003 | Sim |
| Primário | Unicode | 26,774–27,597 | 29,949–32,193 | Não |
| Primário | Linhas curtas | 7,232–7,440 | 7,566–7,727 | Não |

Cinco pares por perfil descrevem essas execuções. Sobreposição não comprova ruído ou equivalência; faixas separadas também não autorizam extrapolação universal. O [manifesto conjunto](manifest.json) preserva todas as amostras, MAD, razões por par e contagens de perdas. Os relatórios brutos [alternativo](alternate/artifact/measurements/report.json) e [primário](primary/artifact/measurements/report.json) permanecem byte a byte iguais aos downloads.

## CPU e RTT de protocolo

Mediana de CPU acumulada pelo processo do terminal em cada carga, em segundos:

| Carga | Alternativo antes → depois | Primário antes → depois |
| --- | ---: | ---: |
| ASCII | 0,38 → 0,35 | 0,53 → 0,52 |
| ANSI | 0,45 → 0,43 | 0,62 → 0,62 |
| Unicode | 0,91 → 0,75 | 1,12 → 0,95 |
| Linhas curtas | 0,99 → 0,89 | 4,28 → 4,09 |

Esses contadores são discretos e não medem instruções. As distribuições completas de CPU estão nos manifestos de cada perfil, [alternativo](alternate/manifest.json) e [primário](primary/manifest.json).

Cada revisão teve 150 observações de RTT DSR por perfil:

| Perfil | Mediana antes → depois, µs | Máximo antes → depois, µs |
| --- | ---: | ---: |
| Alternativo | 50,795 → 49,368 | 127,559 → 3.051,574 |
| Primário | 59,256 → 50,023 | 177,090 → 91,720 |

O pico alternativo de **3,052 ms** ocorreu no candidato, terceiro par, observação 24. Ele continua na amostra e na estatística. Apesar da mediana menor, esse run não demonstra melhoria geral de RTT ou da cauda de latência. No primário, nenhuma observação superou 1 ms. DSR mede respostas de protocolo; não mede apresentação de quadros nem latência entre teclado e tela.

## Ambiente e proveniência

Os dois runs usaram **AMD EPYC 7763**, quatro CPUs lógicas disponíveis, Ubuntu 24.04 x86_64, Rust 1.94.1 e Weston 13 headless com Pixman observado no log. São execuções separadas: o mesmo modelo de CPU não torna os valores absolutos entre perfis uma comparação isolada do efeito do histórico. Compare antes/depois dentro de cada run.

O harness registrou afinidade `[0]`, herdada pelos filhos sem leitura individual de cada terminal. A sessão usou Wayland nativo, sem `DISPLAY`, DejaVu Sans Mono 14 px e imagens desabilitadas. Os 20 processos mantiveram configuração comum dentro de cada perfil, grade de 80×24 células e 720×408 pixels antes e depois das 80 cargas. Foram cinco pares AB/BA/AB/BA/AB por perfil, cerca de 32 MiB por carga e 1 s de estabilização.

O workflow fixado extraiu as revisões com `git archive` e compilou sequencialmente com `cargo build --release --locked`, usando **diretórios de fonte e target separados**, `CARGO_INCREMENTAL=0` e `CARGO_PROFILE_RELEASE_DEBUG=0`. Os logs registram a compilação da aplicação em ambos os diretórios. Os hashes dos executáveis antes/depois são diferentes; Cargo.toml e Cargo.lock coincidem entre revisões, e as árvores Cargo coincidem após normalizar apenas o caminho da aplicação. Os arquivos Cargo e scripts foram conferidos contra as revisões Git exatas.

Foram preservados os **174 arquivos originais** dos artefatos, incluindo `case.json`, `sample.json`, `result.json`, configurações, logs, metadados de processo e proveniência. Relatórios, comandos, histórico, geometria, DSR e estatísticas foram conferidos. Os payloads omitidos no upload foram reproduzidos pelo gerador fixado e verificados por tamanho e SHA-256. O campo bruto `source_refs_verified_by_runner: false` permanece intacto: a origem foi conferida separadamente com Git, arquivos Cargo, workflow e logs dos builds.

Os ZIPs originais não foram baixados para esta auditoria; **seus digests não estão verificados**. Hashes de executáveis são registros do CI, sem os bytes dos binários para rehash independente. Metadados e logs do CI foram fornecidos pelo operador; o auditor retido não atualiza o estado remoto.

Os vinte logs de terminal registram timeout de 100 ms do XDG Settings Portal ao consultar `color-scheme`, embora as amostras tenham terminado com sucesso. Cada log de CI pareado contém 80 ocorrências de `warning:`. Os logs completos e os arquivos `diagnostics.json` de ambos os perfis permanecem no pacote; não há afirmação de ausência de avisos ou erros de subsistemas.

## Validação de correção separada

O [CI 34719026097](https://github.com/guicybercode/kokuban.rs/actions/runs/34719026097), no commit exato `74eaad83dbf813cb15bd883b45d52adf0dc729bd`, concluiu check/test/clippy e a verificação das tabelas Unicode nas duas plataformas. Foram **845 testes Rust no macOS** (604 + 241) e **944 no Linux** (703 + 241), com um benchmark ignorado no Linux, além de 23 testes do harness por plataforma.

Esses gates usam `cargo check/test/clippy --locked --all-targets`; não são testes em perfil release. Os workflows pareados acima compilam os binários release e testam o executor Python, mas não executam os testes Rust de correção. Os [metadados](validation/ci-run.json), o [resumo independente das contagens](validation/summary.json) e o [log integral](validation/ci.log) preservam essa separação.

Os testes cobrem texto misto, larguras de uma e duas células, grafemas contextuais, fragmentação e recuperação de UTF-8 inválido, limites de eventos, inserção, DEC Special Graphics, margens, wrap, histórico, estilos e limites dos blocos de escrita. Smokes Linux também passaram, incluindo clipboard, seleção, imagens e aplicações interativas via SSH. Essa cobertura verifica comportamento no CI; não prova fidelidade ou desempenho equivalente em uma sessão física Omarchy.

## Reproduzir a auditoria dos arquivos retidos

É necessário Python 3.11+ e uma cópia local do repositório contendo as revisões indicadas. A partir da raiz do repositório:

```sh
python3 -B docs/linux-evidence/2026-09-12-mixed-utf8-pairs/audit-paired-run.py \
  --repo . \
  --before 469b2ac3fb88509a5ac4bf1769e4b2b3512c1008 \
  --after 74eaad83dbf813cb15bd883b45d52adf0dc729bd \
  --harness 74eaad83dbf813cb15bd883b45d52adf0dc729bd \
  --run 34721333020 --profile alternate \
  --source-dir docs/linux-evidence/2026-09-12-mixed-utf8-pairs/alternate/artifact \
  --ci-run docs/linux-evidence/2026-09-12-mixed-utf8-pairs/alternate/ci-run.json \
  --ci-log docs/linux-evidence/2026-09-12-mixed-utf8-pairs/alternate/ci.log \
  --output-dir /tmp/kokuban-mixed-pairs-audit-alternate
```

O diretório de saída deve ser novo. Para o primário, use `--run 34721365964 --profile primary`, os três caminhos correspondentes em `primary/` e outro diretório de saída. O auditor não baixa arquivos, compila o terminal ou executa benchmarks; ele lê o Git e os artefatos, regenera payloads e recalcula as estatísticas. Execute essa regeneração fora de medições locais de desempenho.

O [inventário SHA-256 conjunto](evidence-sha256.txt) cobre os arquivos retidos. Os ganhos de processamento Unicode e linhas curtas permanecem acompanhados das perdas individuais, da pequena regressão ANSI primária e do pico de DSR alternativo. Apresentação, equivalência visual, desempenho contra concorrentes e uso físico em Omarchy/Hyprland com GPU e monitor permanecem fora do alcance deste pacote.
