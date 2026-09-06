# Redimensionamento remoto revalidado

O [run 34007583516](https://github.com/guicybercode/kokuban.rs/actions/runs/34007583516)
executou o teste corrigido em `5b1fa95` sobre os APKs de `c41bc56`, no emulador
Android 15/API 35 x86_64. Tanto debug opt1 quanto release passaram de `25 46`
para `4 104` (linhas, colunas) durante a sessão SSH interativa.

O produtor publica a saída de `stty size` por rename e o coletor exige dois
inteiros positivos em cada leitura, além de dimensões diferentes. Isso elimina
o falso positivo de arquivo vazio encontrado no debug anterior `b158779`.
Os JSONs preservam dimensões, checkpoints, run e artifact de origem.

Nos dois perfis, os nove cenários SSH/aplicações e os seis cenários de mídia
passaram, o processo SSH encerrou, o shell local respondeu e as credenciais
temporárias foram removidas sem erros de limpeza. O teste de trust do cenário
release é preparado em debug antes da atualização assinada, conforme os
campos `batch_profile` e `interactive_profile`.

O workflow completo falhou somente no teste IME com a versão antiga do
seletor de layout. Esse resultado não inclui a posterior correção dos glifos
CFF2; o novo APK precisa ser validado após essa alteração de fonte.
