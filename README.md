# sora-continuator

A small Rust helper for stitching Sora 2 video clips together. It speaks the OpenAI Video API, pulls down the rendered MP4s, and shells out to `ffmpeg` to extract the last frame as the seed for the next shot. The crate ships both a library (`sora_continuator`) and a CLI.

## Requirements

- Rust 1.80+
- `ffmpeg` available on your `PATH`
- `OPENAI_API_KEY` exported in your shell

## CLI quickstart

```bash
# create a fresh shot and store it as videos/intro.mp4 + videos/intro.json
just create intro "Wide shot of a teal coupe driving through a desert highway, heat ripples visible."

# continue from the last frame of intro.mp4 for another 12 seconds
just continue-clip intro intro-b "Camera dollies closer as the coupe crests a hill at sunset."

# list everything the tool knows about
just run list

# grab a fresh copy of a rendered asset
just download test-1 video videos/test-1.mp4

# stitch clips together into a single video under videos/test.mp4
just stitch test test-1 test-2
```

Pass `--model sora-2-pro`, `--seconds 12`, etc. by piping through the generic runner, e.g. `just run -- --model sora-2-pro create --id ...`.

Use `just download <id> <variant> <output>` to re-fetch assets. Variants may be `video`, `thumbnail`, or `spritesheet`.

Run `just stitch <id> <clip...>` (or `cargo run -- stitch --id <id> <clip...>`) to concatenate existing clips locally; the result lands at `videos/<id>.mp4`.

## Library overview

```rust
use sora_continuator::{SoraConfig, VideoManager, CreateVideoRequest};

# async context
let manager = VideoManager::new(SoraConfig::default())?;
let clip = manager
    .create_video(CreateVideoRequest {
        local_id: "test-seed".into(),
        prompt: "Macro shot of a vinyl record spinning under neon light".into(),
        model: None,
        size: None,
        seconds: None,
    })
    .await?;
println!("downloaded clip {}", clip.file_path.display());
```

See `just run -- --help` for the full command surface.

## Example Clips

```bash
just create test-1 "A cat walking through a forest"
```

https://github.com/user-attachments/assets/624b82c2-0a4b-4179-a46f-86a2c02f29c5

```bash
just continue-clip test-1 test-2 "The cat stumbles on a gold pocket watch"
```

https://github.com/user-attachments/assets/a85f12d7-21d0-4725-a29d-ab7340d017f7
