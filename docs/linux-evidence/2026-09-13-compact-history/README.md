# Histórico compacto: evidências Linux de 2026-09-13

A compactação das células padrão no fim das linhas de histórico melhorou a
mediana de processamento de linhas curtas em **260,72%**, ANSI em **37,88%** e
Unicode em **8,60%** na tela principal. ASCII principal caiu **1,87%**.
Na alternativa, ANSI caiu **1,92%** e linhas curtas **1,64%**; estas últimas
ficaram mais lentas nos cinco pares. As faixas sobrepostas não demonstram ruído
nem equivalência. O [sumário completo](summary.md) preserva todos os cenários,
faixas, pares, CPU, RTT e limitações.

Fontes medidas: antes `24782c344714727be4f8981cd0d1d10f047aa9f4`, depois e
harness `f0040ad66cbb49d76b387bbe0539c014bc801d28`. A integração posterior da
remoção de dirty tracking não faz parte destes binários.

Runs originais: [principal 34743854765](https://github.com/guicybercode/kokuban.rs/actions/runs/34743854765)
e [alternativa 34743855262](https://github.com/guicybercode/kokuban.rs/actions/runs/34743855262).
Cada run usa cinco pares alternados AB/BA, Rust 1.94.1, fontes e targets isolados,
Ubuntu 24.04 x86_64, EPYC 7763, CPU 0, Weston headless/Pixman e DejaVu Sans Mono
14 px; 80×24 células/720×408 pixels. Os runs são sessões separadas: compare
antes/depois dentro de cada um. O CI funcional
[34743769372](https://github.com/guicybercode/kokuban.rs/actions/runs/34743769372)
passou em macOS e Linux, separadamente das medições.

## Conteúdo preservado

Cada pasta de perfil conserva os 87 arquivos originais do artefato sem alterar
seus bytes, log completo e metadados do CI, fontes exatas do harness, inventário,
manifesto recalculado e diagnósticos. `summary.json` conserva amostras e deltas.
Não são armazenados executáveis ou payloads; os hashes dos executáveis são os
registrados no CI, e os payloads são regenerados pelo auditor. Os ZIPs dos
artefatos e seus digests de API não foram fornecidos à auditoria.

O input abreviado imutável `f0040ad` foi resolvido pelo workflow antes do archive.
O auditor verifica a resolução única no Git, o comando no workflow/log e os
SHAs completos nos artefatos e metadados. Ele não modifica os dados originais.

## Reproduzir a auditoria

Requer Python 3.11+ e clone Git com as duas revisões acima. Na raiz do repositório,
execute para cada perfil, usando uma pasta de saída que ainda não exista:

```sh
python3 -B docs/linux-evidence/2026-09-13-compact-history/audit-compact-run.py \
  --before 24782c344714727be4f8981cd0d1d10f047aa9f4 \
  --after f0040ad66cbb49d76b387bbe0539c014bc801d28 \
  --harness f0040ad66cbb49d76b387bbe0539c014bc801d28 \
  --run 34743854765 --profile primary \
  --source-dir docs/linux-evidence/2026-09-13-compact-history/primary/artifact \
  --ci-run docs/linux-evidence/2026-09-13-compact-history/primary/ci-run.json \
  --ci-log docs/linux-evidence/2026-09-13-compact-history/primary/ci.log \
  --output-dir /tmp/kokuban-compact-primary-audit --repo .
```

Para a alternativa, troque `primary` por `alternate` e o run por `34743855262`.
`evidence-sha256.txt` inventaria o pacote; os inventários internos permitem
verificar cada perfil separadamente. O auditor não acessa a rede, compila ou
abre terminais.

Este ensaio mede processamento PTY e DSR. Não verifica pixels, apresentação,
latência física de entrada, Omarchy/Hyprland em hardware ou posição diante de
outros terminais. RSS é uma fotografia do processo após cargas anteriores,
não memória isolada do histórico nem pico. Os 20 processos preservam o timeout
de 100 ms do portal XDG na inicialização, embora concluam as medições.
