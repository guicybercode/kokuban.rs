# Escrita no PTY esperando POLLOUT: evidências de 2026-09-15

Esperar `POLLOUT` (até 10 ms, rechecando cancelamento) em vez de dormir 1 ms a
cada `EAGAIN` aumentou a mediana da escrita grande no PTY de **0,71 MiB/s** para
**23,07 MiB/s** (variação por par +2919% … +3394%, 0/6 pares mais lentos).
É o caminho usado para colar texto num pane: no macOS a fila de entrada do PTY
comporta cerca de 1 KiB, então cada KiB pagava um sleep.

Fontes: antes `addc796` (benchmark sobre a main `9058b16`), depois `70fdd84`.
Teste `pty::unix::tests::benchmark_large_pty_write` compilado em release (sem
LTO) em targets separados; 8 MiB por amostra, 3 amostras por execução, mediana
por execução, 6 pares AB/BA alternados (`run-pairs.py`).

Host: Apple M4 (10 núcleos, 16 GiB), macOS 26.2, Rust 1.94.1, máquina em uso.

| Execução | Antes (MiB/s) | Depois (MiB/s) |
|---|---:|---:|
| mediana dos 6 pares | 0,71 | 23,07 |
| faixa | 0,71 … 0,72 | 21,67 … 24,81 |

Controle A/A do binário novo (`aa-after.json`): variação por par −6,1% … +4,8%.

## Limitações

- O filho é `sh` em modo raw executando `cat >/dev/null`; aplicações que leem
  mais devagar limitam a escrita por conta própria.
- Com a fila cheia, o cancelamento passa a ser observado em até 10 ms (antes 1 ms).
- Não medido no Linux, onde a fila do PTY é maior; o código é compartilhado e os
  testes de correção rodam nas duas plataformas.
