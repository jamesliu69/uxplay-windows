# H.264 regression fixture

`high-bframes.h264` contains 360 synthetic 64x64 testsrc2 frames at 30 FPS,
independently encoded by FFmpeg 8.1.1 / libx264. It contains High-profile
B-frames and signals BT.709 full range. No phone capture or personal data.

SHA-256: `C9874D5BDA0BB15BE852D3B9BE2F9065986650234FAF37897837A38A89318567`.

Regenerate:

```powershell
ffmpeg -f lavfi -i testsrc2=size=64x64:rate=30 -frames:v 360 -c:v libx264 -preset veryfast -crf 18 -profile:v high -pix_fmt yuv420p -x264-params bframes=3:ref=4:keyint=360:min-keyint=360:scenecut=0:aud=1:repeat-headers=1 -color_range pc -colorspace bt709 -color_trc iec61966-2-1 -color_primaries bt709 -f h264 high-bframes.h264
```

`high-bframes.yuv-samples` contains independent native-FFmpeg-decoded I420
samples in display order: 360 frames, 16 points per frame, three bytes (Y,U,V)
per point, for 17,280 bytes total. Point order is row-major over
`y = [8,24,40,56]`, then `x = [8,24,40,56]`. Chroma uses `(x/2,y/2)` from
the 32x32 U and V planes. Tests apply the BT.709 full-range equations to these
samples and compare every channel with the decoder output.

SHA-256: `EA1E02D7E348DEE3C775EC2912F12AEBB24C674F8ACE3E4BD24B61D63B0D390B`.

To regenerate the samples, decode `high-bframes.h264` with FFmpeg's native
`h264` decoder to `-pix_fmt yuv420p -f rawvideo` (360 frames of 6,144 bytes).
For each frame, read Y at `y*64+x`, U at `4096+(y/2)*32+x/2`, and V at
`5120+(y/2)*32+x/2`, in the point order above, and write the triples.
The source build used was FFmpeg `n8.1.1-7-g3728de467d-20260519`.

FFmpeg is also the production receiver's decoder and is required by its
integration tests. A static executable is bundled beside the engine, or tests
can select it using `UXPLAY_FFMPEG_PATH`.
