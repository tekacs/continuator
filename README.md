# continuator

A small Rust helper for stitching AI generated video clips together. It can talk to OpenAI’s Sora Video API _and_ Google’s Veo 3 Preview on Vertex AI, pulls down the rendered MP4s, and shells out to `ffmpeg` to extract the last frame as the seed for the next shot. The crate ships both a library (`continuator`) and a CLI.

## Requirements

- Rust 1.80+
- `ffmpeg` available on your `PATH`
- For Sora: `OPENAI_API_KEY` exported in your shell
- For Veo: a Google Cloud project with Vertex AI enabled, a location such as `us-central1`, and either
  - `gcloud auth print-access-token` available on your `PATH` (continuator will call it on demand), or
  - a short-lived OAuth token exported as `--gcp-access-token`/`$GCP_ACCESS_TOKEN`

## Installation

```bash
cargo install continuator
```

## CLI quickstart

```bash
# create a fresh shot and store it as videos/intro.mp4 + videos/intro.json
continuator create \
  --id intro \
  --prompt "Wide shot of a teal coupe driving through a desert highway, heat ripples visible."

# continue from the last frame of intro.mp4 for another 12 seconds
continuator continue \
  --from intro \
  --id intro-b \
  --prompt "Camera dollies closer as the coupe crests a hill at sunset."

# list everything the tool knows about
continuator list

# grab a fresh copy of a rendered asset
continuator download \
  --id test-1 \
  --variant video \
  --output videos/test-1.mp4

# stitch clips together into a single video under videos/test.mp4
continuator stitch \
  --id test \
  test-1 test-2

# generate a multi-beat flow and stitched output (creates videos/test-flow.mp4)
continuator flow \
  --id test-flow \
  "Wide shot of a teal coupe" "Camera dollies closer"

# reuse an existing clip as the opener and continue it
continuator flow \
  --id test-flow \
  --start-from intro \
  "Camera glides past" "Sunset silhouette"
```

Pass `--model sora-2-pro`, `--seconds 12`, etc. directly to the relevant subcommand, e.g. `continuator create --model sora-2-pro --id ...`.

To target Veo 3 Preview instead of Sora, add a backend selector and (optionally) GCP metadata:

```bash
continuator create \
  --provider veo \
  --gcp-project my-gcp-project \
  --gcp-location us-central1 \
  --id dune-001 \
  --prompt "Immersive sandstorm rolling across a scorched dune sea, cinematic lighting"

# Veo flow (project/location optional if gcloud defaults are set)
continuator flow \
  --provider veo \
  --gcp-project my-gcp-project \
  --gcp-location us-central1 \
  --id dune-flow \
  "Massive dune eruption" "Scavengers sprint through the storm"
```

If you omit `--gcp-access-token`, the CLI shells out to `gcloud auth print-access-token` for you. `continuator continue --provider veo --from dune-001 --id dune-002 --prompt "..."` automatically captures the final frame of the parent clip and sends it as the first-frame reference. You can drop the project/location arguments entirely if your `gcloud` config already points at the right project and region.

Use `continuator download --id <clip> --variant <variant> --output <path>` to re-fetch assets. Variants may be `video`, `thumbnail`, or `spritesheet`.

Run `continuator stitch --id <output> <clip...>` to concatenate existing clips locally; the result lands at `videos/<output>.mp4`.

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

See `continuator --help` for the full command surface.

## Example Clips (Veo 3 Preview)

```bash
continuator create --provider veo --id test-veo-1 --prompt "A cat walking through a forest"
```

https://github.com/user-attachments/assets/b245bc80-9e5c-4b1b-a0ec-ea332321312d

```bash
continuator continue --provider veo --from test-veo-1 --id test-veo-2 --prompt "The cat stumbles on a gold pocket watch"
```

https://github.com/user-attachments/assets/ca23040c-4837-4e98-88f8-8f74e1f76128

```bash
continuator stitch --id test-veo test-veo-1 test-veo-2
```

https://github.com/user-attachments/assets/6ffdd85e-6598-452f-8a38-e73974e39ee9

## Example Clips (Sora 2)

```bash
continuator create --id test-1 --prompt "A cat walking through a forest"
```

https://github.com/user-attachments/assets/427514aa-6717-4b4d-83d6-fbe4575c4e94

```bash
continuator continue --from test-1 --id test-2 --prompt "The cat stumbles on a gold pocket watch"
```

https://github.com/user-attachments/assets/f1d063c4-39cb-4b48-9b0a-2cb5ec6774cf

```bash
continuator stitch --id test test-1 test-2
```

https://github.com/user-attachments/assets/e8e640ad-a5e2-4995-819e-b4312b926e47
