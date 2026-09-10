# Comparação reproduzível entre terminais

`scripts/compare-terminal-performance.py` executa Kokuban, Ghostty, Alacritty e
Kitty na mesma sessão Linux. Cada janela recebe a mesma aplicação Python por
PTY, os mesmos bytes e uma consulta final de posição do cursor (DSR). O resultado
mede processamento de saída e resposta do protocolo; não mede apresentação de
quadros, latência entre teclado e tela, nem estabelece o melhor terminal.

## Executar no Omarchy

Compile Kokuban em release e mantenha os outros terminais instalados. Em uma
sessão Wayland, fora de tmux/SSH, execute:

```sh
cargo build --locked --release
python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" \
  --backend wayland --samples 7 --bytes 16777216 \
  --environment-note 'Descreva GPU, compositor, monitor e escala desta máquina' \
  --output-dir /tmp/kokuban-comparison-wayland
```

O diretório de saída deve ser novo. Use `--ghostty`, `--alacritty` e `--kitty`
para selecionar executáveis específicos. O padrão solicita todos os quatro:
um executável ausente fica registrado e causa retorno diferente de zero, sem
apagar medições dos terminais disponíveis. Para um ensaio explicitamente parcial,
use, por exemplo, `--terminals kokuban alacritty kitty`.

Para desenvolvimento em X11, um ensaio menor é:

```sh
xvfb-run -a -s '-screen 0 1280x900x24 -noreset' \
  python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" \
  --backend x11 --samples 3 --bytes 2097152 \
  --output-dir /tmp/kokuban-comparison-x11
```

Xvfb e renderização OpenGL por software servem para desenvolver o ensaio. Use a
GPU, o compositor e o monitor reais para conclusões sobre o Omarchy. Mantenha as
janelas visíveis, no mesmo monitor e escala, e suspenda outras compilações ou
medições durante o ensaio. Um gerenciador de janelas pode substituir o tamanho
solicitado; o relatório registra o tamanho efetivo antes e depois de cada carga.

## Cargas e amostragem

Cada amostra abre um processo novo e executa quatro cargas: linhas ASCII, SGR com
cores ANSI/truecolor, texto UTF-8 com caracteres largos e combinantes, e linhas
curtas de um caractere. O tamanho solicitado é arredondado para um número inteiro
de linhas, igualmente para todos os terminais. Os arquivos `.bin` e seus hashes
são preservados. A geração/leitura desses arquivos ocorre antes da medição.

Há estabilização inicial de um segundo, cinco consultas de aquecimento e 30
medições de RTT do protocolo. Antes de cronometrar cada carga, a mesma carga é
executada uma vez para aquecer o parser e as fontes, e a tela é limpa. A medição
termina quando a aplicação recebe a posição esperada após um marcador final.
As respostas DSR são verificadas, inclusive quando a escrita termina antes do
terminal processar todos os bytes.

O programa alterna a ordem dos terminais entre amostras. O padrão usa cinco
amostras; o mínimo é três. `report.json` preserva os tempos individuais, mediana,
mínimo, máximo e desvio absoluto mediano, além da versão e hash dos executáveis,
comandos usados, conteúdo/hash de configuração, geometria do PTY e logs. Também
registra informações de CPU, afinidade e seleção de fonte pelo Fontconfig. Use
`--environment-note` para registrar GPU, compositor e escala; a existência de
uma sessão Wayland ou X11 não comprova aceleração por GPU. O ensaio também
registra CPU e RSS do processo principal do terminal. CPU tem a granularidade
dos ticks de `/proc`; processos auxiliares, Python e compositor não entram nessa
contagem. Para cargas muito rápidas, aumente `--bytes` e o número de amostras.

## Configuração e comparabilidade

O perfil padrão usa a tela alternativa, sem histórico. Isso evita atribuir a
mesma quantidade de linhas a políticas de retenção diferentes. Para investigar
o caminho da tela principal com histórico, execute também:

```sh
python3 scripts/compare-terminal-performance.py \
  --kokuban "$PWD/target/release/kokuban" --backend wayland \
  --screen primary --scrollback-lines 10000 --ghostty-scrollback-bytes 67108864 \
  --samples 7 --bytes 16777216 --output-dir /tmp/kokuban-comparison-history
```

Kokuban, Alacritty e Kitty recebem limite em linhas. Ghostty usa um orçamento em
bytes que também inclui a tela ativa; não há equivalência garantida com 10.000
linhas. O relatório marca essa comparação como não equivalente. As opções de
Ghostty, incluindo isolamento de configuração e execução em processo separado,
seguem sua [referência oficial](https://ghostty.org/docs/config/reference).

Todos recebem DejaVu Sans Mono e uma grade solicitada de 80×24. O tamanho padrão
de Kokuban é 14 pixels lógicos; nos outros terminais, o ensaio usa 10,5 pontos,
equivalentes a 14 pixels a 96 DPI. Escala, arredondamento e métricas podem diferir.
O relatório aponta diferenças nos pixels ou células reais e mudanças de
geometria durante a carga; nessas condições, não se deve ordenar os resultados
como comparação equivalente. `--columns`, `--rows` e `--font-pixels` permitem
repetir o cenário com outro tamanho solicitado.

As configurações são isoladas das preferências do usuário. Alacritty anterior
a 0.13 recebe YAML e versões posteriores recebem TOML, com invocação pela
[CLI documentada](https://alacritty.org/cmd-alacritty.html). Kitty recebe
`--config NONE` e opções explícitas de fonte, janela e histórico, conforme a
[CLI](https://sw.kovidgoyal.net/kitty/invocation/) e a
[configuração oficial](https://sw.kovidgoyal.net/kitty/conf/).

O campo `comparability` informa amostras ausentes, geometria ou retenção
incompatível. Mesmo quando essas verificações passam, a conclusão se limita à
vazão e ao RTT medidos naquele ambiente. O relatório mantém
`rendering_equivalence_verified: false`: a consulta DSR confirma a resposta após
o marcador, mas não verifica o conteúdo do texto nem os pixels apresentados.
Atualmente, as células normais de Kokuban não preservam marcas combinantes;
na carga `e` + acento combinante + espaço, o acento é sobrescrito. Seu atlas
também omite caracteres ausentes na fonte selecionada, sem buscar outra fonte.
Assim, a carga Unicode pode produzir menos conteúdo visual em Kokuban, e seus
tempos não comprovam desempenho superior com a mesma fidelidade de texto.

O programa não gera ranking. Versões
antigas dos concorrentes em uma distribuição de testes não representam suas
versões atuais; a comparação completa exige medições com as versões usadas no
Omarchy, inclusive Ghostty.
