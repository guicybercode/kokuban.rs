# Evidência Android de b158779

Origem: [run 34006719596, tentativa 1](https://github.com/guicybercode/kokuban.rs/actions/runs/34006719596),
Android 15/API 35, Pixel 7 emulado, ABI x86_64. Os JSON preservam as amostras
originais e acrescentam run, commit, artifact e referência de proveniência.
O build ARM64 foi verificado, mas não executado em dispositivo ARM64.

- `controls.json`: sete cenários de entrada com bytes exatos; seleção, colagem,
  scroll, mouse e modificadores. Eventos injetados pelo Android, sem USB físico.
- `lifecycle.json`: shell, Home/retomada na mesma sessão e quatro rotações.
- `release-ssh.json`: aplicações interativas em release; verificação inicial de
  trust e instalação das chaves temporárias em debug antes da atualização
  assinada. Neovim 0.9.5, tmux 3.4, fzf 0.44.1, Git 2.55.0 e Rust 1.94.1 no host.
- `release-media.json`: foto e remoção, Sixel, animação Kitty após o emissor
  encerrar, camadas negativas e vídeo H.264 silencioso decodificado via SSH.
- `release-idle.json`, `release-echo.json`, `video-measurements.json`: CPU/PSS
  do processo e da árvore, mais registros de apresentação do aplicativo.
  O coletor já exclui quadros anteriores ao início da janela de observação.

O resultado de resize **debug** deste run foi descartado: o teste anterior
podia ler um arquivo momentaneamente vazio e interpretá-lo como mudança.
O JSON release contém dimensões completas `25 46` → `4 104` (linhas, colunas),
mas a aprovação do teste corrigido exige nova execução. `5b1fa95` passou a
publicar o arquivo por rename e exigir dois inteiros positivos diferentes.

O gate IME deste run falhou ao localizar a busca de idiomas do Gboard, antes
de testar Hangul. A entrada de `café` já tinha passado. Este conjunto não
comprova composição intermediária nem aprovação de toda a matriz.

## Medições release

APK x86_64: 2.732.519 bytes. O arquivo de proveniência contém seu SHA256 e
os tamanhos das duas bibliotecas. CPU é porcentagem de um núcleo do processo
Android; não inclui o emulador inteiro nem o host SSH.

| Cenário | PSS do app, início → fim | CPU média app / árvore | Apresentações |
| --- | --- | --- | --- |
| Repouso, 15 s após 3 s de espera | 25.586 → 29.712 KiB | 0,2% / 0,2% | zero novos quadros |
| Dez comandos `echo E0`…`echo E9`, 15 s | 29.776 → 30.073 KiB | 14,2% / 14,2% | 77 quadros; 74 correlações de entrada |
| Vídeo 320×180@12 por SSH, amostra de 8 s | 55.875 → 53.352 KiB | 54,66% / 55,54% | 164 quadros; 18,01 chamadas/s |

No eco, a mediana callback→próxima apresentação com saída PTY foi 41,0815 ms,
p95 de 81,8843 ms (`statistics.quantiles(samples, n=100, method="inclusive")[94]`),
e o tempo médio de desenho/apresentação foi 42,6061 ms. No vídeo, a média foi
42,9414 ms e o maior intervalo entre apresentações foi 146,171 ms.

Esses tempos não medem toque ou teclado físico até scanout. As chamadas de
apresentação incluem atualizações do terminal e não equivalem ao FPS do vídeo
exibido. As capturas de vídeo correspondem aos frames decodificados 9, 28, 47
e 65, comprovando mudança dos pixels. O conteúdo é movimento sintético em um
arquivo H.264 real; a foto é NASA AS17-148-22727, com crédito e hash no JSON.

O repouso ainda inclui estabilização de memória após abrir o app. O vídeo
inclui SSH, IME visível e coleta de screenshots. Runners e quantidade de
comandos diferem das medições anteriores; não se atribui uma melhora causal
ao commit de redução de redraws com base nessas amostras.
