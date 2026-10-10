<p align="center">
  <img src="docs/brand/nori-wordmark.svg" alt="nori" width="280">
</p>

<p align="center">
  A music player for your own Navidrome or Subsonic server, on Android, desktop, the terminal and the iPod touch.
</p>

<p align="center">
  <a href="https://github.com/norifm/nori/releases/latest"><img alt="latest release" src="https://img.shields.io/github/v/release/norifm/nori?style=flat-square&label=release&color=3d6b52"></a>
  <img alt="Android · Linux · macOS · terminal · iPod" src="https://img.shields.io/badge/Android%20%C2%B7%20Linux%20%C2%B7%20macOS%20%C2%B7%20terminal%20%C2%B7%20iPod-555?style=flat-square">
  <a href="LICENSE"><img alt="MIT" src="https://img.shields.io/badge/licence-MIT-555?style=flat-square"></a>
</p>

<p align="center">
  <img src="docs/screenshots/home.png" width="28%" alt="Home">&nbsp;
  <img src="docs/screenshots/player.png" width="28%" alt="The player">&nbsp;
  <img src="docs/screenshots/lyrics.png" width="28%" alt="Lyrics">
</p>

- **Sleeps between bursts.** Music is decoded ahead and the CPU sleeps in between; nothing ticks with the screen off.
- **Sounds right.** Gapless albums, AutoMix transitions on the beat, an equalizer with AutoEQ curves, a sound for each output.
- **Finds the words.** Your server's lyrics first, then lyrics services online, scored, word by word where they have it.
- **One core, every screen.** One Rust core plays, queues and decides; each app only draws.
- **Yours.** No accounts, no analytics; your music stays on your server and downloads play offline.

## Install

- **Android**: the APK from the [latest release](https://github.com/norifm/nori/releases/latest), or see [norifm.com](https://norifm.com/#download).
- **Desktop and terminal**: build from source, below.
- **iPod touch** (6th generation, iOS 12, jailbroken): the `.ipa` from the [latest release](https://github.com/norifm/nori/releases/latest), and the [iPod guide](https://norifm.com/docs/clients/ipod/).

## Links

[Website](https://norifm.com) · [Docs](https://norifm.com/docs/) · [Changelog](CHANGELOG.md) · [Contributing](CONTRIBUTING.md)

## Build

```sh
cargo test -j4 --workspace                              # every Rust test
cargo run --release -p nori-desktop                     # the desktop client (nori-cli: the terminal)
./gradlew :app:assembleDebug -PrustTargets=arm64-v8a    # the Android app
```

More in [Building](docs/site/developers/building.md).

## Licence

MIT. See [LICENSE](LICENSE); third-party licences are in [NOTICE](NOTICE) and in the app under Settings, About, Licences.
