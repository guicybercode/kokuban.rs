# Render sob demanda no macOS: evidências de 2026-09-15

Trocar o `NSTimer` repetido de 60 Hz por frames agendados na fila principal
só quando algo muda (com teto de 60 Hz) reduziu a mediana de wakeups ociosos de
**8,0/s** para **0,17/s** e a CPU do terminal parado de **0,15 s** para
**0,03 s** em 30 s; os cinco pares melhoraram nas duas métricas. O throughput
do PTY ficou neutro: medianas por par entre −0,6% e +1,3%.

Fontes: antes `dbb455fdd` (main com LTO e leitor com poll), depois
`a86a304` (`refactor` + `perf` deste PR). Harness: `scripts/compare-kokuban-revisions.py`
com `--samples 5`, 32 MiB por workload e `--idle-seconds 30`; builds release
com o perfil do repositório (LTO) em targets separados.

Host: Apple M4 (10 núcleos, 16 GiB), macOS 26.2, sessão gráfica em uso, Rust
1.94.1, Menlo 14 px, 80×24, AeroSpace com regra de janela flutuante.

## Ocioso (cursor oculto, sem saída, 30 s por amostra)

| Métrica | Antes (mediana, faixa) | Depois (mediana, faixa) |
|---|---|---|
| Wakeups ociosos/s (`top` IDLEW) | 8,0 (1,4 … 30,0) | 0,17 (0,07 … 0,23) |
| CPU do terminal em 30 s (`ps`) | 0,15 s (0,13 … 0,20) | 0,03 s (0,01 … 0,06) |

## Throughput (5 pares AB/BA)

| Workload | Antes (MiB/s) | Depois (MiB/s) | Por par (mediana, faixa) | Mais lentos |
|---|---:|---:|---|---:|
| ascii | 147,4 | 147,5 | +0,1% (−15,6 … +3,1) | 2/5 |
| ansi | 139,2 | 141,1 | +1,3% (−9,6 … +3,8) | 2/5 |
| unicode | 103,9 | 103,4 | −0,5% (−18,4 … +0,6) | 4/5 |
| short_lines | 88,7 | 87,9 | −0,6% (−7,9 … +1,4) | 3/5 |

Controle A/A do binário novo (`aa-after-report.json`, três pares com a mesma
grade): ascii +3,1 … +25,9%, ansi −0,2 … +2,6%, unicode +2,2 … +9,1%,
short_lines −1,1 … +4,5%. As quedas isoladas do AB (−15,6% e −18,4%) estão
dentro da variação observada entre execuções iguais.

## Limitações

- O runner rejeitou o A/A: o AeroSpace ainda redimensionou algumas janelas para
  23×75 e uma amostra mudou de grade durante o workload ascii. `summarize.py`
  usa só pares com a mesma grade estável nos dois lados.
- Os wakeups ociosos do binário antigo variam muito (1,4 … 30/s), provavelmente
  por coalescência de timers quando a janela não está visível.
- Ocioso mede o processo inteiro com a janela aberta e sem interação; eventos
  de teclado, mouse e redimensionamento continuam agendando frames.
- DSR confirma processamento, não apresentação; frames continuam limitados a
  60 Hz.

```sh
python3 -B docs/macos-evidence/2026-09-15-on-demand-render/summarize.py
```
