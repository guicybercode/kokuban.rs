# Experimento local de rejeição antecipada de fronteiras

**A alteração de desempenho deste pacote não foi integrada.** Ela consulta
diretamente as categorias de duas células escalares antes de codificar UTF-8
e consultar o comprimento da linha. Os testes adicionais de Prepend, Hangul,
contexto completo e espaços não escritos foram mantidos em `469b2ac`.

O candidato é `f7ef90f` com [candidate.patch](candidate.patch); a referência
medida é `8dafe9e`. Portanto, o candidato também contém a correção de
Ctrl+Insert: este experimento **não isola** a alteração do grid de possíveis
efeitos de compilação/layout do binário. Uma nova avaliação deve comparar
revisões cuja única diferença de produção seja a otimização.

Os dois executáveis foram compilados com Rust 1.94.1, release e lockfile,
em diretórios de fonte separados, reutilizando dependências em um target
comum. O crate principal foi recompilado em ambos. Os 70 arquivos da fonte
do candidato foram conferidos contra Git mais o patch; o
[manifesto](manifest.json) preserva seus hashes e as estatísticas recalculadas.
Hashes dos executáveis estão nos relatórios; seus bytes não foram arquivados.

A medição usou Docker Linux arm64 sobre macOS, Debian 12, Weston 10.0.1
headless com Pixman, DejaVu Sans Mono 14, afinidade na CPU 0 e grade 80×24
de 720×408 pixels. Cada perfil executou cinco pares AB/BA/AB/BA/AB com
aproximadamente 32 MiB por carga. Compilações locais terminaram antes da
medição Linux; outros trabalhos do host não foram isolados. A triagem
headless macOS coincidiu com compilações e não sustenta as conclusões abaixo.

| Perfil | Carga | Antes, mediana MiB/s | Candidato, mediana MiB/s | Mediana da variação por par | Pares mais lentos |
| --- | --- | ---: | ---: | ---: | ---: |
| Alternativo | ASCII | 148,01 | 153,31 | +2,42% | 1/5 |
| Alternativo | ANSI | 125,53 | 122,94 | −2,75% | 4/5 |
| Alternativo | Unicode | 70,87 | 72,36 | +2,98% | 0/5 |
| Alternativo | Linhas curtas | 68,06 | 67,46 | −0,78% | 3/5 |
| Principal | ASCII | 116,39 | 117,06 | +0,70% | 2/5 |
| Principal | ANSI | 101,69 | 101,67 | +0,97% | 2/5 |
| Principal | Unicode | 60,02 | 63,34 | +5,16% | 0/5 |
| Principal | Linhas curtas | 20,14 | 19,75 | −1,85% | 5/5 |

A variação por par é calculada antes da mediana, não pela divisão das duas
medianas da tabela. Os [dados alternativos](alternate-report.json) e
[principais](primary-report.json) preservam todas as amostras, CPU, RSS,
configurações, payloads e RTT. Há grande dispersão: um par ASCII principal
mostrou +100,84%, outro −9,14%; no alternativo, um par caiu 17,23%.
Os ganhos Unicode não justificam afirmar melhoria geral nem atribuir as
perdas somente à mudança proposta. São observações de processamento PTY/DSR;
não medem apresentação, equivalência visual, teclado até a tela ou Omarchy
físico. O [script usado](measure.sh) registra os argumentos; os caminhos
`/validation` e `/workspace` pertenciam ao container temporário.

A versão experimental passou check/Clippy e 687 testes do executável Linux
mais 225 do exemplo, com um benchmark ignorado. O primeiro container não
tinha init e reteve descendentes zumbis; a suíte completa passou após repetir
com `docker --init`. Os testes de grid mantidos passaram novamente depois
de retirar a otimização. A correção independente de cópia passou no
[CI de `f7ef90f`](https://github.com/guicybercode/kokuban.rs/actions/runs/34668409671),
incluindo o smoke de clipboard Linux; ela permanece integrada.
