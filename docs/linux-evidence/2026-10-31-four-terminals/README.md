# Quatro terminais no mesmo host Linux — 2026-10-31

Medição automática do workflow `linux-terminal-comparison.yml`
([run 41250525735](https://github.com/guicybercode/kokuban.rs/actions/runs/41250525735)),
disparado na `main` em `27bbb15` logo após a integração dos PRs #28–#33
(blocos pixel-aligned, paleta ANSI no macOS, OSC 52, OSC 8, CSI u, busca).
O pacote guarda o `report.json` completo do harness, `summary.json` no mesmo
esquema do pacote de 2026-09-13 (lido por `scripts/render-comparison-chart.py`),
e os registros de CPU, fontes, imagem do container e revisões.

## Medianas de processamento (MiB/s, cinco processos por terminal; entre parênteses mín–máx)

| Carga | Kokuban | Ghostty | Alacritty | Kitty |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 92,361 (86,5–95,4) | 25,551 (24,8–26,2) | 53,755 (52,8–54,3) | 84,496 (83,4–87,1) |
| ANSI | 63,624 (63,4–64,5) | 20,745 (20,4–21,4) | 57,965 (57,3–58,3) | 16,455 (16,2–16,8) |
| Unicode | 47,644 (47,2–47,8) | 26,094 (23,9–26,3) | 55,750 (55,4–56,5) | 59,503 (57,7–64,6) |
| Linhas curtas | 39,634 (38,7–40,2) | 22,384 (22,2–22,7) | 53,128 (52,9–54,1) | 27,788 (26,8–29,7) |

Líder por carga: ASCII **kokuban**, ANSI **kokuban**, Unicode **kitty**,
linhas curtas **alacritty**. Em relação ao ensaio de 2026-09-13 (`d54f801`),
o Kokuban subiu em todas as cargas (ASCII 70,0 → 92,4; ANSI 55,7 → 63,6; Unicode 36,9 → 47,6;
linhas curtas 36,6 → 39,6) e passou a liderar ASCII; os rivais são as mesmas versões.

## Condições

- Host: GitHub Actions x86_64 (Ubuntu 24.04, container Ubuntu 26.04), 4 CPUs lógicas, Weston headless/Pixman, Mesa llvmpipe; `cpu.txt` tem o `lscpu`.
- Versões: Kokuban `kokuban 0.4.0` (commit `27bbb15`, `kokuban-binary-sha256.txt`), Ghostty 1.3.0-dev+0000000, alacritty 0.16.1, kitty 0.45.0 created by Kovid Goyal.
- Geometria 80×24 células, fonte 14 px, tela alternativa sem histórico, ~32 MiB por carga; ordem dos terminais rotacionada entre rodadas; pré-voos de calibração excluídos da mediana.
- DSR mediano do Kokuban: 0.0518 ms (máximo 0.1057 ms) — resposta de protocolo, **não** latência de apresentação.

## Limites

- Mede processamento de saída via PTY até a resposta final do DSR; não mede quadros apresentados, latência tecla-a-tela nem GPU real.
- Um único run, sem auditoria independente como a do pacote de setembro (`verify.py`); os JSONs brutos permitem reproduzir as medianas.
- Resultado restrito a este host e perfil; não estabelece ranking geral em desktops ou no Omarchy/Hyprland físico.
