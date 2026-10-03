# Quatro terminais — Linux x86_64, 13/09/2026

Auditoria **PASS** do [run 34746364220](https://github.com/guicybercode/kokuban.rs/actions/runs/34746364220). Fonte e harness: `d54f801e9dec41a6eb4f159397e45939c367531d`. Este pacote contém os dados do ensaio e permite verificar novamente suas contas e proveniência sem compilar ou medir terminais.

## Resultados no mesmo host

MiB/s, mediana [mínimo–máximo] de cinco processos por terminal:

| Carga | Kokuban | Ghostty | Alacritty | Kitty |
| --- | ---: | ---: | ---: | ---: |
| ASCII | 70,015 [69,109–70,556] | 28,333 [27,970–28,797] | 50,854 [50,160–51,346] | 78,800 [77,039–79,891] |
| ANSI | 55,677 [55,520–56,267] | 22,088 [20,877–22,765] | 53,846 [53,485–54,781] | 16,747 [15,812–16,800] |
| Unicode | 36,853 [36,472–37,197] | 26,646 [26,355–27,307] | 51,779 [51,487–52,745] | 57,365 [56,817–60,094] |
| Linhas curtas | 36,634 [36,523–36,937] | 24,340 [23,301–24,867] | 50,246 [49,417–50,548] | 28,917 [28,636–29,769] |

Kokuban liderou ANSI e superou Ghostty nas quatro cargas. Kitty liderou ASCII e Unicode; Alacritty liderou linhas curtas. As faixas Kokuban/rival são separadas nas 12 comparações deste ensaio. Cinco processos e uma única máquina não estabelecem superioridade global, intervalos de confiança ou desempenho em outros ambientes. Não combinar os números com runs antigos em hosts distintos nem atribuir a diferença a uma otimização isolada.

| Terminal | DSR mediano / máximo (ms) | CPU mediana ASCII / ANSI / Unicode / curtas (s) |
| --- | ---: | ---: |
| Kokuban | 0,074546 / 0,159240 | 0,46 / 0,58 / 0,87 / 0,88 |
| Ghostty | 0,109856 / 2,922494 | 2,53 / 2,79 / 2,50 / 2,44 |
| Alacritty | 0,093647 / 0,557803 | 0,90 / 0,66 / 0,72 / 0,67 |
| Kitty | 3,209762 / 8,825180 | 0,68 / 2,18 / 0,97 / 1,38 |

DSR confirma processamento e resposta do terminal; **não mede apresentação de frames nem latência input-to-photon**. CPU soma `utime+stime` do PID do terminal, com resolução limitada, excluindo filhos e compositor. Não é consumo de energia nem CPU do sistema inteiro.

## Ambiente, versões e controles

AMD EPYC 7763, execução nativa x86_64, afinidade disponível 0–3. Runner Ubuntu 24.04, container Ubuntu 26.04 amd64 e Weston 14.0.2 headless/Pixman. `LIBGL_ALWAYS_SOFTWARE=1` e `GALLIUM_DRIVER=llvmpipe` foram solicitados; o `GL_RENDERER` efetivo de cada terminal não foi registrado. Não houve medição em Omarchy/Hyprland físico com GPU e monitor.

Kokuban 0.2.0; Alacritty 0.16.1; Kitty 0.45.0. **Ghostty 1.3.0-dev+0000000, canal tip**, pacote Ubuntu `1.3.0~us1-0ubuntu1.1`: este resultado não caracteriza a release estável 1.3.0. Versões completas, pacotes, hashes registrados e imagem estão em `evidence/ci-report.json`, `evidence/system-packages.txt` e `evidence/container-image.json`.

Todos os 20 processos cronometrados usaram tela alternativa, histórico zero, estabilização de 1 s, 80×24 células, 720×408 pixels e células 9×17. Oito preflights (quatro iniciais e quatro ajustados) ficaram fora das estatísticas; as geometrias iniciais dos concorrentes eram diferentes. A ordem dos terminais foi rotacionada nas cinco rodadas. São 80 cargas medidas e 600 RTT; oito probes e 240 RTT de calibração foram excluídos.

DejaVu Sans Mono: Kokuban 14 px e demais 10,5 pt; ajustes de célula horizontal/vertical: Kokuban 0/0, Ghostty 1/1, Alacritty 1/0, Kitty 1/0. Noto CJK e Color Emoji instaladas e identificadas por hashes. Geometria e fontes disponíveis iguais não provam pixels, escolha de fallback, graphemes ou fidelidade Unicode iguais.

Payloads de aproximadamente 32 MiB foram regenerados da função pura no Git fixado e conferidos por tamanho/SHA-256; configs e argv dos 28 processos também foram reconstruídos. O marcador final DSR foi `1b5b313b3552`. `evidence/manifest.json` preserva distribuições e validações completas.

## Proveniência e limites da auditoria

Cargo.toml/lock, harness e workflow coincidem com o Git fixado. O log mostra build release com Rust 1.94.1 em `/source` somente leitura e target separado `/build/target`. Hash do executável Kokuban registrado pelo CI: `8d43ff7da9e5d00c1425c9d9d7d6ae4b5f1dfdd01e46f66b708db72296444c8f`. Os executáveis, bytes das fontes tipográficas e tar integral da fonte não vieram no download; seus hashes registrados não constituem rehash independente desses arquivos.

Passaram 977 testes Rust (729 principais + 248 do exemplo; um ignorado), 52 testes Python e smoke Wayland nativo com cwd/argv literais e TTY/DSR. Os logs completos retêm avisos de portal no Kokuban; GTK/acessibilidade/DPI/cell_size no Ghostty; systemd/portal/notificações no Kitty. Não impediram os gates, mas seu impacto no tempo não foi quantificado.

## Seleção e integridade dos arquivos

`evidence/` preserva **223 arquivos, 1.400.205 bytes**, byte a byte como entregues pela auditoria. São 214 arquivos originais — 196 arquivos dos 28 processos (case, child, sample, result, finish, log e configuração), 17 metadados/logs e o relatório bruto — mais nove arquivos: metadados e log do CI, quatro fontes de harness/workflow obtidas do Git fixado, auditor, observações de logs e manifesto.

O download tinha **837 arquivos, 46.209.731 bytes**. `original-download-inventory.json` mantém caminhos, tamanhos e SHA-256 de todos. `retention-selection.json` mapeia cada original retido ao destino e explica os **623 omitidos**: 616 caches gráficos gerados (45.572.191 bytes) e sete configurações padrão vazias criadas pelo Ghostty. As sete configurações explícitas usadas pelo benchmark continuam retidas. Os caches não são necessários para auditar os dados registrados; sua omissão impede reconstruir aquele estado exato de cache para repetir a execução.

Não foi necessária compressão. `evidence/manifest.json` verifica todos os outros 222 arquivos internos. `package-sha256.json` verifica todos os demais arquivos deste pacote, inclusive o manifesto interno e o inventário completo. Os checksums detectam alterações; não são uma assinatura independente de autenticidade.

`auditor-version.patch` e `auditor-source.json` documentam a adaptação do auditor anterior somente para run, revisão, versão, totais de testes e data. `summary.json` guarda os cálculos derivados; `verification.json` registra a reauditoria do pacote final.

## Reauditar

Requisitos: Python 3, Git e checkout deste repositório que contenha a revisão `d54f801e9dec41a6eb4f159397e45939c367531d` e `docs/linux-evidence/2026-09-11-boundary-terminals/manifest.json`. Esse manifesto antigo é consultado apenas para distinguir o hash do executável; suas medições não entram nas estatísticas.

A partir desta pasta:

```sh
python3 -B verify.py --repo /caminho/para/kokuban.rs
```

O comando valida inventários, seleção, hashes, snapshots contra Git e executa o auditor retido. Não acessa rede, não modifica Git, não compila e não mede terminais. Para verificar também os bytes de todos os originais, incluindo os omitidos, se o download ainda estiver disponível:

```sh
python3 -B verify.py --repo /caminho/para/kokuban.rs --original-artifact /caminho/para/download
```

Para reproduzir diretamente o relatório do auditor:

```sh
python3 -B evidence/audit.py --artifact evidence --ci-run evidence/ci-run.json --ci-log evidence/ci.log --repo /caminho/para/kokuban.rs
```

Os snapshots em `evidence/harness/` documentam como o CI conduziu o ensaio original; os comandos acima apenas auditam resultados já existentes.
