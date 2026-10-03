# Cor do glifo no cache: ensaio pareado Linux x86

O [run 34744896010](https://github.com/guicybercode/kokuban.rs/actions/runs/34744896010) passou. A auditoria verificou as fontes e compilações, os 120 resultados e 12 pares de frames byte a byte. A conclusão e todos os resultados estão em [summary.md](summary.md); estatísticas e deltas individuais estão em [audit.json](audit.json).

- Base: `24782c344714727be4f8981cd0d1d10f047aa9f4`.
- Candidato: `965843d698edb94ac705f28c7948d26adef3edf2`.
- Harness comum: `4a80b7e5d42fa9a4f205a5c42b06648eb7a77948`.
- Benchmark injetado SHA-256: `b4e01fbfb366231b737074eb10c2d7a05f7cc87107bc054859e3b41410ed256a`.
- Executável de teste antes, hash registrado no CI: `b592cceaa5aa4df2842ca8339e9e485d67b2ee392bf3a9951dd2180a05f4b056`.
- Executável depois: `d3dd15331b1785ada901fbb3ab009bb16ceb55479c70e259e5b5f4ecb06a3991`.

São cinco pares de processos em ordem alternada, 120 amostras de 300 frames medidos e 30 frames de aquecimento por modo. CPU AMD EPYC 7763, Linux x86_64 nativo, afinidade 0, DejaVu Sans Mono 14 e fontes Noto CJK/Color Emoji. Geometria 120×40 células, 1080×680 pixels. Todos os oito casos de emoji/Unicode reduziram o tempo nos cinco pares, com faixas separadas. ASCII puro, tela inteira e desenho integral: **+0,314% de tempo**, 3/5 pares mais lentos, com sobreposição. A regressão local ARM de aproximadamente 5% não se reproduziu nessa magnitude neste host; sua causa não foi comprovada como ruído.

O relógio inclui pintura na CPU e cálculo de danos no modo incremental. Exclui criação do snapshot, rasterização inicial dos glifos, PTY, compositor/apresentação e latência até o monitor. Os modos e conteúdos seguem ordem fixa dentro do processo; somente as versões alternam entre pares. Os resultados não devem ser combinados com os da VM ARM nem extrapolados para outros terminais. O CI Rust completo de correção é separado deste benchmark.

A integração posterior entrou na `main` em `d54f801e9dec41a6eb4f159397e45939c367531d`, após 863 testes locais, check e Clippy. O [CI 34746312645](https://github.com/guicybercode/kokuban.rs/actions/runs/34746312645) passou em macOS e Linux. Esses testes validam a integração; os tempos acima continuam referentes às revisões medidas.

## Arquivos preservados

[artifact.tar.xz](artifact.tar.xz) contém **todos os 67 originais**, incluindo os dois tars de fontes originais, todos os 24 frames brutos (12 por versão), 10 logs de processos, relatório com 120 records, mensagens JSON e logs de compilação, provas da injeção, scripts/workflow fixados, revisões, toolchain, CPU, configuração/fontes e hashes dos arquivos tipográficos. Os bytes originais totalizam 104.005.019 bytes.

O pacote usa um único TAR.XZ determinístico, com 3.142.112 bytes, que comprime repetições entre os arquivos. A mudança é somente de armazenamento: todos os membros foram recuperados e comparados com os originais. O TAR externo tem ordem lexical, mtime/uid/gid 0, mode 0644, nomes de usuário/grupo vazios; LZMA2 preset 6, dicionário 32 MiB, CRC64. Os tars internos mantêm seus bytes originais.

[archive.json](archive.json) registra tamanhos e hashes do TAR externo e do XZ. [artifact-inventory.json](artifact-inventory.json) registra tamanho/SHA-256 de cada original. [evidence-sha256.txt](evidence-sha256.txt) cobre os arquivos deste pacote. [provenance.json](provenance.json) retém as provas dos 1.167 blobs por revisão, prefixos runtime preservados, compilações novas e targets distintos. [ci-run.json](ci-run.json) identifica o run e seus passos concluídos.

Os bytes dos executáveis e dos arquivos tipográficos não foram incluídos pelo workflow; seus hashes são observações de CI. A afinidade foi lida no harness e herdada pelos filhos, sem leitura separada por processo. A igualdade de pixels cobre estas seis fixtures, não todos os textos/estilos possíveis.

## Verificar e recuperar sem medir novamente

A partir da raiz do repositório, com Python 3.11+ e as três revisões disponíveis localmente:

```sh
python3 -B docs/linux-evidence/2026-09-13-glyph-color/verify.py --repo .
```

O script verifica o pacote, descomprime em memória, confere os 67 originais, compara cada blob dos tars com os commits Git, verifica o benchmark/prefixos e as compilações novas, recalcula as 120 amostras e compara os 12 pares de pixels e mais oito equivalências ASCII antes/depois de carregar emoji. Usa apenas leituras Git; não compila, não executa terminais, não faz medições nem acessa a rede.

Para também recuperar os 67 arquivos em um diretório novo:

```sh
python3 -B docs/linux-evidence/2026-09-13-glyph-color/verify.py \
  --repo . --restore /private/tmp/kokuban-glyph-color-originals
```

O destino deve estar ausente. Sem `--repo`, o script ainda verifica bytes, hashes, registros, estatísticas e pixels, mas não compara as fontes ao banco de objetos Git. A auditoria original e a validação deste pacote foram feitas com `--repo`.
