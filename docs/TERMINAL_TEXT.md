# Preservação de texto no terminal

O grid guarda grafemas estendidos, conforme o [Unicode UAX #29](https://unicode.org/reports/tr29/). Uma letra com acento combinante, uma bandeira ou um emoji com modificador e ZWJ mantém sua sequência UTF-8 completa, mesmo quando os bytes chegam em leituras diferentes do PTY. A largura é calculada para o conjunto por `unicode-width`; as duas células de um grafema largo selecionam o mesmo texto uma única vez.

## Cópia

Cada linha registra o conteúdo escrito e se continua na próxima por quebra automática. A seleção une essas continuações, preserva quebras explícitas e espaços escritos, e omite o preenchimento vazio da tela. Portanto, uma URL ou comando longo não recebe `\n` apenas porque ultrapassou a largura da janela. O limite de bytes da área de transferência considera o grafema completo antes de adicioná-lo.

## Redimensionamento

O resize reorganiza linhas lógicas completas, incluindo o histórico. Caracteres largos passam juntos para a linha seguinte; numa grade de uma coluna, seu conteúdo permanece guardado para cópia e posterior expansão. Linhas que deixam a área visível são retidas, inclusive quando há texto abaixo de um cursor perto do topo. O comando Selecionar tudo no Linux inclui esse conteúdo retido.

A tela principal mantém seu conteúdo quando uma aplicação usa a tela alternativa. Os cursores acompanham o texto durante o reflow. Como as coordenadas físicas mudam, seleções existentes são invalidadas: selecione novamente após redimensionar.

Diminuir a janela não expulsa conteúdo já retido apenas por criar mais quebras automáticas. Saída posterior continua sujeita aos limites do histórico; comandos explícitos de apagamento e sobrescrita continuam tendo seu efeito normal.

## Desenho

O atlas usa rustybuzz para formar os glifos de cada grafema, incluindo substituições e posição dos acentos. Metal e o renderer Linux recebem o texto completo. Fontes alternativas instaladas fornecem caracteres ausentes da fonte principal. Apple Color Emoji e Noto Color Emoji têm testes de formação e rasterização de bandeiras, keycaps, modificadores e sequências ZWJ. As fontes não são distribuídas com o aplicativo; no Linux, o pacote `fonts-noto-color-emoji` fornece a fonte utilizada no CI.

O atlas mantém cobertura monocromática e pixels RGBA para emojis coloridos. Isso acrescenta 4 MiB ao atlas em memória CPU; a textura Metal passa de 1 MiB para 4 MiB.

## Verificação reproduzível

```sh
cargo test --locked --all-targets
cargo clippy --locked --all-targets
```

`src/content_preservation_tests.rs` exercita o decoder usado pelo PTY, seleção e resize em conjunto: URLs e comandos, cortes em todos os limites de bytes UTF-8, acentos, ZWJ, bandeiras, keycaps, espaços escritos, histórico, cursores e tela alternativa. Testes específicos do atlas verificam os glifos formados e seus pixels.

Em Linux com Xvfb, xdotool, xclip, xwd e fontes instaladas:

```sh
cargo build --locked
xvfb-run --auto-servernum --server-args='-screen 0 800x600x24' \
  python3 scripts/linux-clipboard-smoke.py "$PWD/target/debug/kokuban"
```

O teste lê o clipboard real após copiar grafemas e uma URL longa. Depois muda a largura para 13, 55, 20 e 40 colunas e exige os mesmos bytes, sem reimprimir a saída. Também mantém as verificações de colagem, seleção visual e seleção com Shift durante rastreamento do mouse.
