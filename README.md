# continuator

A small Rust helper for stitching AI generated video clips together. It can talk to OpenAI’s Sora Video API _and_ Google’s Veo 3 Preview on Vertex AI, pulls down the rendered MP4s, and shells out to `ffmpeg` to extract the last frame as the seed for the next shot. The crate ships both a library (`continuator`) and a CLI.

## Requirements

- Rust 1.80+
- `ffmpeg` available on your `PATH`
- For Sora: `OPENAI_API_KEY` exported in your shell
- For Veo: a Google Cloud project with Vertex AI enabled, a location such as `us-central1`, and either
  - `gcloud auth print-access-token` available on your `PATH` (continuator will call it on demand), or
  - a short-lived OAuth token exported as `--gcp-access-token`/`$GCP_ACCESS_TOKEN`

## CLI quickstart

```bash
# create a fresh shot and store it as videos/intro.mp4 + videos/intro.json
just create-sora intro "Wide shot of a teal coupe driving through a desert highway, heat ripples visible."

# continue from the last frame of intro.mp4 for another 12 seconds
just continue-sora intro intro-b "Camera dollies closer as the coupe crests a hill at sunset."

# list everything the tool knows about
just run list

# grab a fresh copy of a rendered asset
just download test-1 video videos/test-1.mp4

# stitch clips together into a single video under videos/test.mp4
just stitch test test-1 test-2
```

Pass `--model sora-2-pro`, `--seconds 12`, etc. by piping through the generic runner, e.g. `just run -- --model sora-2-pro create --id ...`.

To target Veo 3 Preview instead of Sora, add a backend selector and GCP metadata:

```bash
just create-veo dune-001 "Immersive sandstorm rolling across a scorched dune sea, cinematic lighting" my-gcp-project us-central1
```

If you omit `--gcp-access-token`, the CLI will shell out to `gcloud auth print-access-token` for you. `just continue-veo dune-001 dune-002 "..." my-gcp-project us-central1` automatically captures the final frame of the parent clip and sends it as the first-frame reference. You can drop the project/location arguments entirely if your `gcloud` config already points at the right project and region.

Use `just download <id> <variant> <output>` to re-fetch assets. Variants may be `video`, `thumbnail`, or `spritesheet`.

Run `just stitch <id> <clip...>` (or `cargo run -- stitch --id <id> <clip...>`) to concatenate existing clips locally; the result lands at `videos/<id>.mp4`.

## Library overview

```rust
use continuator::{ContinuatorConfig, ProviderKind, VideoManager, CreateVideoRequest};

# async context
let manager = VideoManager::new(ContinuatorConfig {
    provider: Some(ProviderKind::Veo),
    model: Some("veo-3.0-generate-preview".into()),
    gcp_project: Some("my-gcp-project".into()),
    gcp_location: Some("us-central1".into()),
    ..ContinuatorConfig::default()
})?;
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
just create-sora test-1 "A cat walking through a forest"
```

https://github.com/user-attachments/assets/624b82c2-0a4b-4179-a46f-86a2c02f29c5

```bash
just continue-sora test-1 test-2 "The cat stumbles on a gold pocket watch"
```

https://github.com/user-attachments/assets/a85f12d7-21d0-4725-a29d-ab7340d017f7
