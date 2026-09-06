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

Validação local do produtor em2026-09-05: MP4 H.264 decodificado em12 quadros,
269176 bytes de protocolo; dois testes passaram (fragmentação/reconstrução
zlib e rejeição de quadros incompletos). Isso prova o produtor, não a reprodução
no Android. Foto real, vídeo apresentado, atualização/remoção e medidas em
release ainda precisam de evidência do dispositivo.

Referência: [protocolo gráfico Kitty](https://sw.kovidgoyal.net/kitty/graphics-protocol/).
