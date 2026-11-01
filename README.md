# Kiku

Kiku shows live captions for what each app on your PC says, such as a call, a stream
or a video, and for your microphone. Everything runs on your PC: no account, no cloud, no telemetry.

**[Download Kiku for Windows](../../releases/latest/download/Kiku-setup.exe)** (Windows 10 and 11, 64-bit)

## Features

- **A caption section per app** (Discord, a browser, a game, ...) plus one for your microphone, in a small
  always-on-top panel; quiet apps fold away by themselves.
- **Captions mode:** the newest lines as subtitles over the screen, in the font, size and colours you pick.
- **Dictation:** hold Ctrl+Win to type what you say into any app; paste or undo it with a key. Every
  shortcut can be changed.
- **Offline speech models:** Parakeet (English, or 25 languages) and Moonshine, on the CPU or an NVIDIA
  GPU. Setup recommends one for your PC and downloads it; you can also add your own sherpa-onnx or
  Moonshine model.
- **History:** search past captions by words, app or day, or with SQL. Lines are kept for as many days
  as you choose, or only when you press save.
- **For assistants on your PC:** an MCP server (`kiku-mcp.exe`) lets a local AI assistant read and search
  your captions, if you set it up.
- Themes, a light and dark look, and per-app blocking.

## Installing

Run `Kiku-setup.exe`. It installs Kiku for you alone, without admin rights. The installer isn't
code-signed, so Windows SmartScreen may say it "protected your PC": choose **More info**, then
**Run anyway**. Kiku checks for updates once a day if you leave that on, and asks before installing one.

## Privacy

What Kiku hears, its captions and its transcripts stay on your PC. Kiku goes online only to download the
models you pick (from Hugging Face, NVIDIA, Microsoft's ONNX Runtime releases and this repository's
releases) and, if *Check for updates* is on, once a day to read the latest version from this repository's
releases. Nothing else is sent.

## Building from source

You need Windows 10 or 11, [Rust](https://rustup.rs) (stable, MSVC), Visual Studio 2022's C++ build tools,
CMake and git. In PowerShell 7, from the repository:

```
./scripts/fetch-runtime.ps1      # sherpa-onnx's ONNX Runtime and NVIDIA's CUDA libraries, into third_party/
./scripts/build-moonshine.ps1    # the Moonshine runtime, into third_party/
cargo build --release
```

`target\release\kiku.exe` then runs with the libraries beside it. Models are downloaded from the app's
settings. `./scripts/fetch-runtime.ps1 -CpuOnly` skips the 2 GB of NVIDIA libraries if you won't use a GPU.

A release build like the published one, and its installer ([Inno Setup 6](https://jrsoftware.org/isinfo.php)):

```
./scripts/build-release.ps1 -Test
iscc /DVersion=0.1.0 installer/kiku.iss
```

`.github/workflows/release.yml` builds and publishes each release from this repository's source.

## Licence

MIT: see [LICENSE](LICENSE). Kiku is built with, ships with and downloads software and models by others
under their own licences, listed with their terms in `THIRD-PARTY-NOTICES.txt` in its install folder
(made by `scripts/notices.py` from `installer/notices.txt`).
