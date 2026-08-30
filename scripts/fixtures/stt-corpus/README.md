# STT replay corpus

This directory contains ten validated English clips from Mozilla Common Voice
Corpus 11.0. Common Voice 11.0 is released under the
[CC0 1.0 public-domain dedication](https://creativecommons.org/publicdomain/zero/1.0/).
The source audio and reference rows were read from the ungated
`akahana/common-voice-11-eng-sample` mirror at revision
`4354744379973dd44a1b2273d7beb893810912f5`:

- `en_test_0.tar`
- `test.tsv`

Each source MP3 was converted without trimming to mono, signed 16-bit PCM WAV
at 16 kHz. The `.txt` with the same stem is the reference transcript from the
validated Common Voice test row. `manifest.json` pins the source metadata plus
every WAV and transcript digest; the benchmark also pins the manifest digest
in code. It rejects missing pairs, an unexpected file count, any byte drift,
or WAVs outside that exact PCM contract.

The WAV SHA-256 digests after conversion are:

```text
f65f99f19db20afae32ca57add79a9ad0e07dd8aa754e76c5c8808d14af9feeb  common_voice_en_17263741.wav
9858d3797da024684ab75d0781043e6f751c187872b9cd4caa6a9f1b84e3f04a  common_voice_en_17561821.wav
71587ddb03fbb1125aa6551053b3f03539bc4e071553b690626fe039046c6c26  common_voice_en_17893917.wav
e64e52980f3d0787668bcb2066802cf8421d30f19e91b4c2ae461e638fd271fc  common_voice_en_18132047.wav
39b8d90e946630953b660760c910085ac9f58d25ffcaab7e4d2621dc6571c5af  common_voice_en_21953345.wav
f138918c59de77db4a598aeafae4db9b2fe1d739576f002e36c2ab1c5123d42a  common_voice_en_27340672.wav
00391cd00b9875ec72c215f226a5385bc0579e8302a2ef6b19a4f4f873e5c011  common_voice_en_27710027.wav
277831ab8a339cd6f2bca43ef3de2c347a89be85f91e8def1f05a7050939a64c  common_voice_en_30533383.wav
795d9e14c493285e740c4c572575c54b6b62980287ef350c878df4d8888ffce0  common_voice_en_59751.wav
9b64bd70854ade53fe75bf5376c690077f727da54ac03c67e47f395894d0cdc5  common_voice_en_699711.wav
```
