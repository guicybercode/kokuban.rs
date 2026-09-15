# Leitor do PTY com poll no macOS: evidências de 2026-09-15

Trocar a varredura com `sleep(2 ms)` do leitor do PTY do macOS por `poll` sem
timeout, com leitura fora dos locks e um único lock atlas → árvore por wakeup,
aumentou a mediana de throughput de ponta a ponta em **392,6%** (ASCII),
**262,3%** (ANSI), **51,3%** (Unicode) e **20,0%** (linhas curtas), sem par
mais lento. Em repouso, a mediana de wakeups ociosos caiu de **93,8/s** para
**11,9/s** e a CPU do terminal em 20 s caiu de **0,25 s** para **0,11 s**; os
cinco pares melhoraram nas duas métricas.

Fontes medidas: antes `28b1f212d5b8754d89a896b3bb7568080d9af75a` (main após o
PR #13), depois `555f8f733bf7a4a7301600f421ac5d9b67000923`. Harness:
`scripts/compare-kokuban-revisions.py` da main `2140020`, com `--samples 5`,
32 MiB por workload e `--idle-seconds 20`. Builds release sem LTO em targets
separados; hashes dos executáveis em `summary.json`.

Host: MacBook Apple M4 (10 núcleos, 16 GiB), macOS 26.2, sessão gráfica em uso,
Rust 1.94.1, fonte Menlo 14 px, grade 80×24 na tela alternativa. O AeroSpace
estava ativo com uma regra que deixa as janelas do kokuban flutuantes.

## Resultados

| Workload | Antes (MiB/s) | Depois (MiB/s) | Variação por par (mediana, faixa) | Pares mais lentos |
|---|---:|---:|---|---:|
| ascii | 27,5 | 139,5 | +392,6% (+274,5 … +479,6) | 0/5 |
| ansi | 37,0 | 134,0 | +262,3% (+176,7 … +288,8) | 0/5 |
| unicode | 63,2 | 94,7 | +51,3% (+46,3 … +76,3) | 0/5 |
| short_lines | 66,4 | 80,0 | +20,0% (+7,8 … +32,4) | 0/5 |

Ocioso (cursor oculto, sem saída, 20 s por amostra):

| Métrica | Antes | Depois | Depois − antes por par |
|---|---:|---:|---|
| Wakeups ociosos/s (`top` IDLEW) | 93,8 | 11,9 | −56,5 … −102,0 |
| CPU do terminal (s em 20 s, `ps`) | 0,25 | 0,11 | −0,12 … −0,15 |

## Controles A/A

- **Binário antigo** (`aa-before-report.json`): muito ruidoso. Nos três pares
  com a mesma grade, ASCII variou −70,2% … −7,9% e ANSI −60,3% … +5,3% entre
  execuções do mesmo executável. O leitor antigo depende do agendamento do
  `sleep`; ainda assim, todos os pares AB ficaram acima da maior variação A/A.
- **Binário novo** (`aa-after-report.json`): estável. Nos quatro pares com a
  mesma grade, as variações ficaram entre −5,1% e +6,8% (medianas −1,9% … −0,5%).

## Limitações

- O runner rejeitou os dois A/A porque uma amostra observou 23×75 em vez de
  24×80: o AeroSpace ainda redimensiona algumas janelas. `summarize.py`
  recalcula os pares usando só os que tiveram a mesma grade estável nos dois
  lados e registra quais foram usados. O relatório AB passou sem exclusões.
- A máquina estava em uso. No A/A novo, o par 5 caiu para ~37 MiB/s com
  0,1 wakeup/s ocioso, compatível com a janela num workspace oculto
  (App Nap); esse par foi o excluído por geometria.
- `top` IDLEW conta wakeups do processo inteiro; os ~12–20/s restantes vêm
  sobretudo do timer de render fixo de 60 Hz, não do leitor.
- DSR confirma o processamento pelo terminal, não a apresentação na tela. O
  throughput de ponta a ponta inclui render, locks e o agendamento do macOS.

## Reproduzir o sumário

```sh
python3 -B docs/macos-evidence/2026-09-15-poll-reader/summarize.py
```

Os relatórios preservam as amostras individuais; caminhos locais de artefatos
foram substituídos por `<artifacts>` e `<harness>`. Payloads são regenerados
pelo harness e não são armazenados.
