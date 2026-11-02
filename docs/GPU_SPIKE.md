# Spike: apresentador GPU (wgpu) para a janela Linux

Pergunta do spike: vale trocar o rasterizador de CPU da janela Linux
(softbuffer + `draw.rs`) por um apresentador GPU via wgpu, mantendo o caminho
de software como fallback? Resultado: abstração `FramePresenter` em uso pelo
caminho atual, apresentador wgpu completo e compilando atrás da feature Cargo
`gpu` (desligada por padrão; `[render] backend` padrão `"software"`), CI verde
sem a feature, workflow de medição rodado em Mesa (sem GPU real) e
recomendação ao fim deste documento.

Branch: `spike/wgpu-linux` (PR #49). Tempo gasto: cerca de um dia de agente.
Desenvolvido num Mac: o código Linux foi verificado por `cargo check`/`clippy`
cruzados para `aarch64-unknown-linux-gnu`, o shader WGSL por `naga`, e só rodou
na CI do GitHub (Xvfb, Weston headless, Mesa lavapipe/llvmpipe). **Nenhum
quadro foi apresentado numa GPU real.**

## Arquitetura

```
present_frame (linux_window.rs)
  ├─ trava grid → graphics, snapshot_locked_grid, seleção, graphics.snapshot
  ├─ layout do preedit do IME e área do cursor do IME
  └─ SceneFrame { snapshot, images, preedit, cell_dimensions, colors,
                  background, frame_size, detect_visible_content }
        │
        ▼  FramePresenter::resize + present  (presenter.rs)
  ┌─────────────────────────┐   ┌──────────────────────────────────────┐
  │ SoftwarePresenter       │   │ WgpuPresenter (gpu.rs + gpu.wgsl)    │
  │ softbuffer Surface      │   │ Instance (Vulkan|GL) → Surface →     │
  │ buffer.age → frame_damage│  │ Adapter (LowPower) → Device/Queue    │
  │ draw_grid_snapshot_in_band│ │ FrameBuilder: quads instanciados     │
  │ draw_ime_preedit        │   │ atlas RGBA8 1024² (upload se dirty)  │
  │ histórico PresentedScene│   │ imagens: texturas RGBA8, cache LRU 64 │
  │ saída idêntica à anterior│  │ 1 render pass, clear = background    │
  └─────────────────────────┘   └──────────────────────────────────────┘
```

- `SceneFrame` é dono do snapshot e das imagens: o apresentador de software os
  move para o histórico de dano sem copiar, exatamente como antes. O trait
  também expõe `invalidate()` (paleta/atlas mudaram), `supports_partial_damage`
  e `backend_name`.
- `linux_window.rs` mudou o mínimo: campos `surface`/`context`/`presented_scenes`
  viraram `presenter: Option<Box<dyn FramePresenter>>`, `create_window` chama
  `create_presenter`, `present_frame` monta a cena e delega. O resto fica em
  arquivos novos para não colidir com o trabalho de múltiplos painéis.
- Feature Cargo `gpu` (`Cargo.toml`): `wgpu` e `pollster` são opcionais no
  alvo Linux; `gpu.rs`, o shader e o ramo `WgpuPresenter` só compilam com
  `cargo build --features gpu`. Sem a feature, `backend = "gpu"` falha ao
  iniciar com `kokuban was built without the gpu feature; rebuild with
  --features gpu` e `auto` usa software.
- Seleção do backend (`presenter.rs::create_presenter`): `[render] backend`
  (`config.rs`, tabela com `#[serde(default)]`) ou `KOKUBAN_RENDER`.
  - `software` (padrão): nunca cria instância wgpu.
  - `auto`: tenta o wgpu; cai para software se não há adapter, se a
    criação do device falha, ou se o adapter é rasterizador de CPU
    (`DeviceType::Cpu` ou nome com llvmpipe/lavapipe/softpipe/swiftshader).
    Uma linha `Linux renderer: ...` em nível info registra a escolha e o motivo.
  - `gpu`: aceita qualquer adapter (inclusive lavapipe, é assim que a CI
    exercita o pipeline) e falha com `kokuban: failed to initialize Linux
    window: could not initialize the GPU renderer requested by ...`.

### O que o apresentador wgpu desenha

Mesma ordem de camadas de `draw_grid_snapshot_in_band` (repaint completo):
imagens com `z < i32::MIN/2`, fundos de célula (só `bg != Default` ou
REVERSE), imagens com `z < 0`, por célula box drawing/blocos via
`builtin_rects` (retângulos, sem atlas) ou glifo do atlas (`fill = 1` máscara
`fg * tex, tex.a`; `fill = 2` glifo colorido) e sublinhados (single, double,
curly, dotted, dashed reproduzidos de `draw_cell_underline_in_band`), imagens
com `z >= 0`, cursor (bloco/barra/sublinhado, alpha 180 sem cor configurada,
glifo recortado dentro do cursor de bloco com cor configurada) e por fim o
preedit do IME (`draw_ime_preedit`). Blending source-over com alpha direto
(`BlendState::ALPHA_BLENDING`); formato de superfície `Bgra8Unorm` ou
`Rgba8Unorm` de preferência, com view não-sRGB quando só há formato sRGB, para
produzir os mesmos bytes que o caminho de CPU. Sampler linear no atlas (quads
alinhados a texel → equivalente a nearest) e nearest nas imagens (mesma regra
de centro de pixel de `draw_image_rgba`). `PresentMode::AutoVsync`,
`CompositeAlphaMode::Opaque`.

Ainda não mirrorado: nada de `draw.rs` ficou de fora, mas os testes de pixel
só cobrem o caminho de CPU. Há testes unitários para a classificação de
adapters de CPU, escolha de formato, layout dos instances e recorte de glifo
no cursor (`gpu.rs::tests`); a saída real do shader não tem teste.

### Modo smoke

`KOKUBAN_EXIT_AFTER_FIRST_FRAME=1` no wgpu: se a superfície anuncia
`COPY_SRC`, o quadro é copiado para um buffer mapeável e varrido por pixels
diferentes do fundo (mesma verificação da CPU). Sem `COPY_SRC`
(`visible_content = None`) o smoke **falha** com "could not verify the first
frame" em vez de passar sem verificar. Os smokes existentes da CI (`ci.yml`)
compilam sem a feature e rodam com o padrão `software`; continuam verdes e o
grep de isolamento de dependências não muda.

## Pegada de dependências

`cargo tree --target aarch64-unknown-linux-gnu`: 154 crates únicos sem a
feature (igual à `main`; nenhum wgpu/naga/ash/glow/pollster/khronos-egl) → 188
com `--features gpu` (+34). Novos com a feature: wgpu, wgpu-core, wgpu-hal, wgpu-types, naga, naga-types,
wgpu-naga-bridge, wgpu-core-deps-windows-linux-android, ash, gpu-allocator,
presser, glow, khronos-egl, renderdoc-sys, profiling, codespan-reporting,
spirv, bit-set, bit-vec, half, ordered-float, hashbrown 0.17, foldhash 0.2,
allocator-api2, rustc-hash 1.1, static_assertions, num-traits, document-features,
litrs, syn 3, bytemuck_derive, zerocopy-derive, cfg_aliases 0.2.2 (sobe de
0.2.1), pollster. Nenhum nome casa com o grep proibido da CI
`(objc2|block2|core-foundation|core-graphics|core-text|drm) v` — a feature
`drm` do wgpu fica desligada e o crate `drm` não entra na árvore; o grep não
precisa mudar. As bibliotecas do sistema (libvulkan, libEGL) são carregadas em
tempo de execução (`ash` `loaded`, `khronos-egl` `dynamic`), então o binário
não ganha dependências de link e segue rodando em máquinas sem Mesa.

Tamanho do binário release: +5,46 MB só com `--features gpu`; sem a feature
volta ao da `main` (ver "Medições"; o Mac não linka o alvo Linux, o workflow
mede).

## Medições

Workflow manual `.github/workflows/linux-gpu-smoke.yml` (`workflow_dispatch`;
só aparece para o `gh workflow run` depois de estar na `main`; durante o spike
foi disparado por um gatilho `pull_request` temporário) em `ubuntu-24.04`,
build com `--features gpu`, Mesa
(`mesa-vulkan-drivers` = lavapipe, `libgl1-mesa-dri` = llvmpipe):

Primeira execução (run `41492287911`, commit `d568832`, artefato
`linux-gpu-smoke`), `ubuntu-24.04`, Mesa 25.2.8, LLVM 20.1.2:

| Cenário | Adapter escolhido | Resultado |
| --- | --- | --- |
| Xvfb, `KOKUBAN_RENDER=gpu` | Vulkan `llvmpipe (LLVM 20.1.2, 256 bits)` (lavapipe), `Bgra8Unorm`, readback on | quadro apresentado e **verificado** (conteúdo visível na leitura de volta); saída 0 |
| Xvfb, só GL (`VK_ICD_FILENAMES=/nonexistent`) | GL `llvmpipe`, OpenGL 4.5 Core via EGL, `Bgra8Unorm` | quadro apresentado; no `d568832` a verificação falhou por falta de `COPY_SRC` no swapchain GL; com `b0a5950` (leitura de uma textura offscreen) o run `41492945452` apresentou e **verificou** o quadro também no GLES |
| Xvfb, `KOKUBAN_RENDER=auto` | — | caiu para **software** (adapter de CPU rejeitado), quadro verificado |
| Weston headless `--renderer=gl`, `gpu` | Vulkan lavapipe (o GL do compositor e o Vulkan do cliente são independentes) | quadro apresentado e verificado; saída 0 |
| Weston headless `--use-pixman`, `gpu` | Vulkan lavapipe | quadro apresentado e verificado; saída 0 |

Ou seja: o caminho wgpu apresenta no X11 e no Wayland (xdg-shell via winit)
com os dois backends do wgpu, e o `auto` faz o que promete sem GPU. O run
`41502844019`, já com a feature e o padrão `software`, repetiu os cinco
cenários com o mesmo resultado (comparação: software 0,54 ms/quadro e 47 MiB,
wgpu sobre lavapipe 4,9 ms/quadro e 174 MiB — o host da CI varia entre runs,
a proporção não).

`compare-render-backends.py`, binário release, 3 rodadas alternadas, 20 000
linhas coloridas com pausa de 2 ms a cada 200 linhas, Xvfb 1280×900, janela
80×24 (`DejaVu Sans Mono` 14, célula 9×17):

| backend | rodou como | CPU (mediana) | quadros | CPU/quadro | wall | RSS pico |
| --- | --- | --- | --- | --- | --- | --- |
| software | software | 0,320 s | 304 | 1,07 ms | 0,73 s | 46 MiB |
| gpu | wgpu (Vulkan lavapipe) | 0,680 s | 68 | 10,0 ms | 0,76 s | 174 MiB |

Leitura: com um rasterizador de CPU a conta inclui o Mesa inteiro (JIT LLVM,
rasterização, cópia para o X), então o `gpu` custa 2× a CPU do processo, 4×
o RSS e apresenta 4,5× menos quadros no mesmo intervalo (vsync do lavapipe
coalescendo redesenhos). Isso **não** diz quanto custaria numa GPU real; diz
que o fallback do `auto` é obrigatório e que a montagem da cena na CPU por si
só não aparece como gargalo nesse tamanho de janela (o wall é igual).

Tamanho do binário release (x86_64, `lto = "fat"`, `codegen-units = 1`, run
`41502844019`, commit `f413cbd`):

| Build | `cargo tree -e normal` (linhas) | Binário release |
| --- | --- | --- |
| `main` (`f0c9ca2`) | 173 | 8 345 520 bytes |
| spike sem a feature (build normal, o que a CI e um release levariam) | 173 | 8 362 176 bytes (**+16,7 KB**, a abstração e a seleção de backend) |
| spike com `--features gpu` | 221 | 13 806 496 bytes (**+5,46 MB, +65 %**) |

O custo com a feature é wgpu-core/hal, naga (parser WGSL e backends
SPIR-V/GLSL) e ash/glow ligados estaticamente; sem `gles` cairia parte disso,
e um `strip` no empacotamento não muda a proporção. O workflow falha se
wgpu/naga/ash/glow aparecerem na árvore sem a feature. No alvo aarch64 contado
localmente: 154 nomes únicos sem a feature (igual à `main`), 188 com.

`scripts/compare-render-backends.py` roda o mesmo binário com
`KOKUBAN_RENDER=software` e `=gpu`, uma carga de rolagem idêntica via `-e`, e
amostra `utime+stime` e `VmHWM` do processo em `/proc` até ele sair; a linha
`Linux renderer <backend>: presented N frames` (emitida no `Drop` dos
apresentadores) normaliza CPU por quadro. Num host com rasterizador de CPU os
números do `gpu` incluem o trabalho do Mesa, então servem para comparar o
custo da montagem da cena, não o ganho de uma GPU real.

## Riscos e limitações

- **Nenhum teste em hardware real.** Wayland (Hyprland/Sway/GNOME/KDE), X11
  com drivers proprietários, HiDPI com `ScaleFactorChanged` e janelas
  redimensionadas só foram exercitados logicamente. Por isso o wgpu fica
  atrás de `--features gpu` e o padrão é `software`: ninguém recebe o caminho
  GPU sem pedir duas vezes (compilar e configurar).
- **Custo de arranque em `auto`:** criar a instância wgpu (carregar libvulkan,
  enumerar ICDs, inicializar EGL) e descartá-la no fallback adiciona dezenas de
  milissegundos ao primeiro quadro em máquinas sem GPU.
- **Atlas 1024×1024 sem despejo:** igual ao caminho de CPU, mas o upload é da
  textura inteira (4 MiB) a cada glifo novo (`dirty`); em sessões com muitos
  grafemas/emoji novos isso pode custar mais que o rasterizador de CPU. O
  próximo passo natural é upload por região (`write_texture` com origem) ou
  por faixa (`row_height`).
- **Imagens:** uma textura por `(image_id, ponteiro dos pixels, tamanho)` com
  teto de 64 entradas; animações kitty re-enviam a textura a cada quadro novo;
  imagens maiores que `max_texture_dimension_2d` são puladas com aviso em vez
  de desenhadas parcialmente.
- **Sem dano parcial:** cada quadro reconstrói todos os instances na CPU
  (O(células)) e redesenha tudo; em telas grandes o custo de CPU da montagem
  pode se aproximar do repaint de banda do caminho de software. Medir antes de
  otimizar (instances persistentes por linha, só linhas sujas).
- **Erros de validação do wgpu** são logados (`on_uncaptured_error`) e não
  derrubam a janela; um quadro errado passaria despercebido fora do smoke.
- **GLES:** o backend GL do wgpu exige GLES 3.0/GL 3.3 via EGL; em máquinas
  só com GLX antigo o `auto` cai para software.
- `window.opacity` continua ignorado no Linux (janela opaca nos dois caminhos).

## Recomendação

**GO condicional, em duas partes:**

1. **GO para a abstração `FramePresenter` e para o apresentador wgpu como
   opt-in (`--features gpu` + `backend = "auto"|"gpu"`).** A separação
   cena/apresentação já paga por si: o
   `present_frame` encolheu, o caminho de software ficou intacto (mesmos
   bytes, mesmos testes) e o trabalho de múltiplos painéis em
   `linux_window.rs` não precisa saber quem rasteriza. O wgpu compila, passa
   em clippy, apresenta e é verificável no X11 e no Wayland com Vulkan e GLES.
2. **NO-GO para ligar a feature por padrão ou tornar `auto` o padrão antes de
   medir em hardware real.** Nada aqui rodou numa GPU; o único número
   comparável (lavapipe) é pior que o software em CPU, RSS e cadência. O PR já
   reflete isso: feature desligada, padrão `"software"`, binário e árvore de
   dependências dos builds normais iguais aos da `main`. Só promover (feature
   no build de release, depois `auto` como padrão) quando as medições abaixo
   existirem.

Trabalho restante estimado para o GO completo (feature no release, padrão
`auto`):

| Item | Estimativa |
| --- | --- |
| Rodar `compare-render-backends.py` e os smokes em 2–3 máquinas reais (Intel/AMD Mesa no Wayland e X11, NVIDIA proprietário), com janelas grandes (200×60, HiDPI 2×) e carga de imagens kitty; registrar em `docs/linux-evidence/` | 1 dia |
| Upload parcial do atlas (região suja em vez de 4 MiB) e `dirty` por faixa | 0,5 dia |
| Instances persistentes por linha / só linhas sujas, ou dano parcial com `LoadOp::Load` quando a superfície preserva conteúdo | 1–2 dias |
| Testes de pixel compartilhados: renderizar a mesma cena pelos dois apresentadores num teste `ignore` que roda no workflow (readback já existe) e comparar com tolerância | 1 dia |
| Tratamento de perda de device (`DeviceLost` → recriar ou cair para software em tempo de execução), redimensionamento sob `Outdated`, `ScaleFactorChanged` | 0,5 dia |
| Medir e, se preciso, adiar a sondagem do `auto` (custo de arranque em máquinas sem GPU) | 0,5 dia |
| Documentação de usuário (`docs/INSTALL.md`, `kokuban.toml`), CHANGELOG final, decidir se os artefatos de release Linux levam `--features gpu` | 0,5 dia |

Total: cerca de 5–6 dias de agente além deste spike. Se as medições em
hardware real não mostrarem ganho claro em janelas grandes ou com imagens, a
recomendação passa a ser manter só a abstração e remover o `gpu.rs` (e as 34
dependências) antes de cristalizar.
