# Validação de entrada, SSH e aplicações Linux

Os testes em `scripts/linux-apps-smoke.py` e `scripts/linux-clipboard-smoke.py` abrem o Kokuban em Xvfb e enviam eventos XTest para a janela. Não injetam a entrada diretamente no PTY. O workflow Linux instala as ferramentas de teste e guarda evidência da sessão SSH em um artefato do CI.

## SSH e aplicações

O teste inicia um `sshd` temporário, sem privilégios, escutando apenas em `127.0.0.1`. Gera chaves de cliente/servidor exclusivas para a execução e fixa a chave pública do servidor no `known_hosts` do teste. A tentativa com uma chave incorreta precisa falhar antes de executar o comando remoto. A conexão correta exige verificação estrita e autenticação por chave. Configurações e credenciais existentes do usuário não são utilizadas.

A sessão aberta pelo terminal verifica PTY remoto, `TERM`, teclado, mudança de 80×24 para 88×27, SIGWINCH e interrupção por Ctrl+C. Depois usa aplicações interativas reais: insere e salva texto Unicode no Neovim, escolhe `beta` no fzf, divide um tmux pelo teclado e executa um comando na nova área. Arquivos de resultado, versões, logs e screenshots registram os passos; as chaves temporárias não entram nos artefatos. O encerramento deve terminar a sessão SSH e a janela normalmente.

Esses cenários verificam operações concretas das versões instaladas no runner. Não comprovam todos os plugins, aplicações, servidores, métodos de autenticação, Wayland ou Android. SSH Linux utiliza o cliente disponível no sistema. A aplicação do terminal permanece Rust; OpenSSH, editores e ferramentas de teste são programas externos.

## Clipboard e seleção

O teste de clipboard verifica bytes UTF-8, colagem com e sem bracketed-paste, normalização de quebras, atalhos e cópia de uma seleção com o mouse. Um aplicativo com mouse SGR habilitado verifica o caminho normal de mouse; Shift-drag deve selecionar localmente sem transmitir uma sequência incompleta para esse aplicativo. A seleção tem destaque visual.

O backend mantém a propriedade do clipboard enquanto o terminal está aberto e faz chamadas ao sistema fora da thread da janela. A fila do backend e as respostas de paste pendentes têm limites. O texto recebido e a seleção copiada têm limite de 1 MiB; o paste codificado inclui seu enquadramento nesse limite. O backend arboard retorna uma String completa antes da checagem de tamanho, portanto a alocação transitória da leitura do sistema ainda é controlada por essa biblioteca.

A seleção acompanha o descarte de linhas antigas do histórico. Mudanças de tela, reset e alterações de coordenadas invalidam seleções incompatíveis. Paste pendente é validado contra a tela e o modo de colagem de origem; repinturas da mesma tela não o cancelam. Ctrl+C/Ctrl+V comuns continuam destinados ao aplicativo.

Wayland exige um compositor com `ext-data-control-v1` ou `wlr-data-control`, ou acesso via XWayland. O teste de CI é X11. Cópia de linhas automaticamente quebradas ainda usa as linhas físicas: o Grid não registra esse vínculo. Grafemas compostos completos, OSC 52 e medições de consumo continuam como trabalho restante.

## Executar os cenários

Em um Linux com as dependências do workflow instaladas, execute como usuário comum:

```sh
cargo build --locked
xvfb-run -a -s '-screen 0 1280x900x24 -noreset' timeout 180s \
  python3 scripts/linux-apps-smoke.py "$PWD/target/debug/kokuban" \
  --artifacts-dir /tmp/kokuban-apps-evidence
xvfb-run -a -s '-screen 0 800x600x24' timeout 90s \
  python3 scripts/linux-clipboard-smoke.py "$PWD/target/debug/kokuban"
```

Consulte o resultado do CI correspondente ao commit: a presença dos scripts, por si só, não comprova uma execução bem-sucedida. Vídeo, desempenho sustentado e compatibilidade abrangente precisam de evidências próprias.
