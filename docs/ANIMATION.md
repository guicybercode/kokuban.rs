# Animação Kitty compartilhada

O Linux usa o cache de imagens por CPU para uploads `a=f`, controle `a=a`, composição `a=c` e exclusão de frames `a=d,d=f/F`. A identificação aceita `i` ou `I`; continuações podem repetir `a=f` ou omitir a ação. Uploads aceitam Base64 com ou sem padding, em blocos codificados de até 128 KiB. O parser interpreta os campos segundo a ação final e rejeita parâmetros numéricos inválidos.

O comportamento segue o [protocolo Kitty](https://sw.kovidgoyal.net/kitty/graphics-protocol/#animation) e o código da versão 0.48.2. Para composição, `r` seleciona a origem e `c` o destino, conforme a implementação do cliente/terminal. Uploads aceitam `C` para composição, com precedência sobre o alias `X` descrito no protocolo. Respostas bem-sucedidas a uploads de frame incluem `r`; controle de reprodução bem-sucedido é silencioso. Arquivos compartilhados via memória continuam sem suporte. Transmissão por arquivo depende da configuração existente.

Cada frame retém um canvas RGBA completo. Todos contam no orçamento de bytes do cache, com limites adicionais de 256 frames por imagem e 4096 frames no cache. O frame apresentado compartilha o mesmo `Arc`, sem uma cópia permanente adicional. Decodificação, composição e snapshots ainda vivos podem causar alocações transitórias além do orçamento retido. Falhas de validação preservam os frames existentes.

A reprodução suporta estados parado, carregando e executando; escolha explícita do frame; intervalos; repetição finita ou infinita; e frames sem duração. O relógio usa `Instant` e calcula atrasos longos sem iterar uma vez por ciclo perdido. No Linux, imagens fora da área visível e janelas oclusas ou sem superfície não solicitam timers. O próximo acesso visível atualiza a posição da reprodução. O renderer Metal mantém imagens estáticas e responde `ENOTSUP` às operações de animação.

## Integração Android

A segunda sessão mantém seu adaptador de imagens em `android_images`. Para incorporar os commits desta implementação, inclua também o módulo irmão:

```rust
#[path = "renderer/image_animation.rs"]
mod image_animation;
```

Os módulos de cache e handler referenciam `super::image_animation`, portanto não exigem `crate::renderer`. O parser fornece as estruturas de comando tipadas. O cache CPU expõe:

```rust
store.advance_animations(now: Instant, visible: &HashSet<ImageId>) -> AnimationUpdate
// AnimationUpdate { changed: bool, next_deadline: Option<Instant> }
```

Calcule os IDs visíveis usando os mesmos retângulos, scrollback e viewport utilizados para desenhar. Respeite a ordem dos locks: grid e depois cache; solte-os antes da apresentação. Solicite redraw quando `changed` for verdadeiro; use `ControlFlow::WaitUntil` para o menor prazo retornado, ou `Wait` quando não existir prazo. Suspenda os timers enquanto a janela ou superfície Android estiver indisponível. Não crie uma thread de reprodução nem um polling contínuo. `src/software_graphics.rs` e `src/linux_window.rs` mostram a integração Linux.

## Verificação

Os testes do núcleo usam relógio determinístico para intervalos, repetições, carregamento, exclusão, composição e orçamento de memória. Os testes Linux passam bytes de protocolo pelo decoder, cache e snapshot. `scripts/linux-graphics-smoke.py` verifica pixels reais em Xvfb, incluindo uma sequência sintética com formato semelhante ao icat que continua depois de o emissor parar de escrever. Esse teste não representa execução do binário icat, nem mede FPS ou consumo de energia.

Execute `cargo run --example graphics -- animate` dentro do Kokuban Linux para enviar 30 frames, iniciar três repetições e encerrar o emissor. A aplicação continua responsável pela reprodução. Android precisa incorporar e validar este contrato em dispositivo; a existência do núcleo compartilhado não comprova reprodução Android. Vídeo geral, áudio e medições de desempenho continuam pendentes.
