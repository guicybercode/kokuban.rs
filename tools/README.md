# Mídia via Kitty

`kitty-media.py` usa FFmpeg no computador ou host SSH para decodificar uma
foto/vídeo. Envia quadros RGBA comprimidos com zlib em transmissões Kitty de
até 4096 bytes base64, substituindo o mesmo ID de imagem. O Android usa seu
parser/handler/compositor compartilhado. FFmpeg não é incluído no APK.

```sh
python3 tools/kitty-media.py photo.jpg --photo --width 640 --height 360 --hold 10
python3 tools/kitty-media.py clip.mp4 --width 320 --height 180 --fps 12 --duration 10 --stats producer.json
```

Pelo terminal Android, execute o segundo comando no host SSH onde estiverem
Python, FFmpeg e o arquivo. A configuração Kokuban deve permitir gráficos
Kitty. O padrão de 320x180 a 12 quadros/s é um cenário inicial de medição,
não uma promessa de desempenho. O protocolo aceita outras dimensões, porém
este produtor limita saída a 1280x720/30fps/600s para manter trabalho limitado.

Esta rota reproduz vídeo sem áudio; não implementa sincronização audiovisual
nem a extensão nativa de animações Kitty. Animações/GIF também passam pelo
decodificador FFmpeg e usam substituições completas de quadros. Ao terminar,
o produtor remove sua imagem e restaura o cursor. Ele usa ID19001 por padrão;
`--image-id` permite evitar colisão com outro produtor na mesma sessão.

O JSON mede quadros/bytes e tempo do produtor. `display_fps` permanece nulo:
FPS apresentado, CPU e memória exigem coleta no dispositivo.

Fixture reproduzível, sem conteúdo de terceiros:

```sh
ffmpeg -f lavfi -i testsrc2=size=160x90:rate=12 -t 1 -c:v libx264 -pix_fmt yuv420p sample.mp4
python3 tools/kitty-media.py sample.mp4 --width 160 --height 90 --fps 12 --duration 1 --hold 0 --stats producer.json
python3 -m unittest discover -s tools -p 'test_*.py'
```

Validação local do produtor em 2026-09-05: MP4 H.264 decodificado em 12 quadros,
269176 bytes de protocolo; três testes passaram (fragmentação/reconstrução
zlib, rejeição de quadros incompletos e decodificação de uma fotografia). O teste
de foto encontrou e corrigiu um filtro de FPS que descartava seu único quadro.
O produtor também foi executado por SSH no emulador Android 15/API 35 x86_64,
em debug e release de `b9ba8b8`: fotografia, remoção e vídeo passaram nos
testes de pixels. O MP4 H.264 320×180 a 12 quadros/s apresentou capturas
correspondentes aos frames decodificados 0, 16, 31 e 46 em release.
Os [resultados e capturas](../docs/android-evidence/b9ba8b8/results.json)
identificam o APK e o cenário; não validam automaticamente versões posteriores.

`media-photo.json` identifica a fotografia real AS17-148-22727, da tripulação
Apollo 17, com crédito NASA Johnson Space Center e hash dos bytes utilizados.
A validação baixa a imagem para `target`, compara os pixels enviados com 25
pontos da captura Android e verifica sua remoção. Consulte a
[fonte da fotografia](https://science.nasa.gov/earth/earth-observatory/the-blue-marble-from-apollo-17-1133/)
e as [orientações de uso da NASA](https://www.nasa.gov/nasa-brand-center/images-and-media/).

`scripts/android/media_scenarios.py` também reutiliza a sequência de teste
Linux para Sixel e animação Kitty nativa, que avança após encerrar o emissor.
O cenário de vídeo decodifica um MP4 H.264 com movimento sintético, compara
capturas com os pixels dos frames decodificados e coleta CPU/PSS e chamadas
de apresentação. Na amostra release de oito segundos, a PSS do aplicativo
foi de 49.964 a 63.194 KiB e a CPU média foi de 47,25% de um núcleo.
As 20,08 chamadas de apresentação por segundo incluem redesenhos do terminal;
não representam FPS efetivamente exibidos do vídeo. Consulte os
[dados brutos](../docs/android-evidence/b9ba8b8/video-measurements.json).

Referência: [protocolo gráfico Kitty](https://sw.kovidgoyal.net/kitty/graphics-protocol/).
