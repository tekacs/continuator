use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use continuator::{
    ContinueVideoRequest, CreateVideoRequest, ProviderKind, SoraConfig, VideoManager, VideoVariant,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about = "Generate and extend Continuator video slices via the CLI", long_about = None)]
struct Cli {
    /// Video generation backend (sora or veo).
    #[arg(long, global = true, value_enum)]
    provider: Option<ProviderKind>,

    /// Override the OpenAI API key. Defaults to the OPENAI_API_KEY environment variable.
    #[arg(long, global = true)]
    api_key: Option<String>,

    /// Default Sora model (e.g., sora-2 or sora-2-pro).
    #[arg(long, global = true)]
    model: Option<String>,

    /// Default output size (e.g., 1280x720).
    #[arg(long, global = true)]
    size: Option<String>,

    /// Default clip length in seconds.
    #[arg(long, global = true)]
    seconds: Option<u32>,

    /// Directory for storing videos and metadata (defaults to ./videos).
    #[arg(long, global = true)]
    data_dir: Option<PathBuf>,

    /// Poll interval in milliseconds when waiting for renders.
    #[arg(long, global = true)]
    poll_interval_ms: Option<u64>,

    /// Google Cloud project id for Veo.
    #[arg(long, global = true)]
    gcp_project: Option<String>,

    /// Google Cloud location for Veo (for example, us-central1).
    #[arg(long, global = true)]
    gcp_location: Option<String>,

    /// Pre-fetched Google Cloud access token for Veo requests.
    #[arg(long, global = true)]
    gcp_access_token: Option<String>,

    /// Cloud Storage URI to store Veo outputs instead of returning bytes.
    #[arg(long, global = true)]
    gcp_storage_uri: Option<String>,

    /// Whether Veo should generate audio (defaults to true).
    #[arg(long, global = true)]
    gcp_generate_audio: Option<bool>,

    /// Preferred Veo resolution (720p or 1080p).
    #[arg(long, global = true)]
    gcp_resolution: Option<String>,

    /// Whether Veo should let Gemini enhance prompts (defaults to true).
    #[arg(long, global = true)]
    gcp_enhance_prompt: Option<bool>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a brand-new clip.
    Create {
        /// Local identifier used for filenames (e.g., intro-001).
        #[arg(long)]
        id: String,
        /// Prompt describing the clip.
        #[arg(long)]
        prompt: String,
        /// Override the model for this clip.
        #[arg(long)]
        model: Option<String>,
        /// Override the size for this clip.
        #[arg(long)]
        size: Option<String>,
        /// Override the duration in seconds.
        #[arg(long)]
        seconds: Option<u32>,
    },
    /// Generate a sequence of clips from multiple prompts and stitch them.
    Flow {
        /// Base identifier used for generated clip filenames and final stitch.
        #[arg(long)]
        id: String,
        /// Optional starting clip to continue from.
        #[arg(long)]
        start_from: Option<String>,
        /// Override the model for generated clips.
        #[arg(long)]
        model: Option<String>,
        /// Override the size for generated clips.
        #[arg(long)]
        size: Option<String>,
        /// Override the duration in seconds for generated clips.
        #[arg(long)]
        seconds: Option<u32>,
        /// One or more prompts describing each beat of the flow.
        #[arg(required = true)]
        prompts: Vec<String>,
    },
    /// Generate a continuation clip using the last frame of an existing video.
    Continue {
        /// Local identifier of the clip to extend.
        #[arg(long = "from")]
        parent_id: String,
        /// Local identifier to assign to the new clip.
        #[arg(long)]
        id: String,
        /// Prompt defining the next beat of the scene.
        #[arg(long)]
        prompt: String,
        /// Override the model for this clip.
        #[arg(long)]
        model: Option<String>,
        /// Override the size for this clip.
        #[arg(long)]
        size: Option<String>,
        /// Override the duration in seconds.
        #[arg(long)]
        seconds: Option<u32>,
    },
    /// List locally stored clips and continuations.
    List,
    /// Download alternate assets (thumbnail or spritesheet) for a clip.
    Download {
        /// Local identifier of the clip.
        #[arg(long)]
        id: String,
        /// Asset variant to download.
        #[arg(long, value_enum)]
        variant: AssetVariant,
        /// Output path for the asset.
        #[arg(long)]
        output: PathBuf,
    },
    /// Concatenate local clips into a single output MP4.
    Stitch {
        /// Local identifier to assign to the stitched clip output file.
        #[arg(long)]
        id: String,
        /// One or more clip identifiers to concatenate (positional arguments).
        #[arg(required = true)]
        clips: Vec<String>,
    },
}

#[derive(Debug, Clone, clap::ValueEnum)]
enum AssetVariant {
    Video,
    Thumbnail,
    Spritesheet,
}

#[tokio::main]
async fn main() -> Result<()> {
    setup_tracing();

    let cli = Cli::parse();

    let config = SoraConfig {
        provider: cli.provider,
        api_key: cli.api_key,
        model: cli.model,
        size: cli.size,
        seconds: cli.seconds,
        data_dir: cli.data_dir,
        poll_interval_ms: cli.poll_interval_ms,
        gcp_project: cli.gcp_project,
        gcp_location: cli.gcp_location,
        gcp_access_token: cli.gcp_access_token,
        gcp_storage_uri: cli.gcp_storage_uri,
        gcp_generate_audio: cli.gcp_generate_audio,
        gcp_resolution: cli.gcp_resolution,
        gcp_enhance_prompt: cli.gcp_enhance_prompt,
    };

    let manager = VideoManager::new(config).context("failed to construct video manager")?;

    match cli.command {
        Command::Create {
            id,
            prompt,
            model,
            size,
            seconds,
        } => {
            let metadata = manager
                .create_video(CreateVideoRequest {
                    local_id: id.clone(),
                    prompt,
                    model,
                    size,
                    seconds,
                })
                .await?;

            print_metadata(&metadata);
        }
        Command::Continue {
            parent_id,
            id,
            prompt,
            model,
            size,
            seconds,
        } => {
            let metadata = manager
                .continue_video(ContinueVideoRequest {
                    parent_local_id: parent_id,
                    local_id: id.clone(),
                    prompt,
                    model,
                    size,
                    seconds,
                })
                .await?;

            print_metadata(&metadata);
        }
        Command::Flow {
            id,
            start_from,
            model,
            size,
            seconds,
            prompts,
        } => {
            if prompts.is_empty() {
                anyhow::bail!("flow requires at least one prompt");
            }

            let start_clip = start_from.clone();
            let mut previous = start_from;
            let mut generated_ids = Vec::new();

            for (index, prompt) in prompts.into_iter().enumerate() {
                let clip_local_id = format!("{}-{:02}", id, index + 1);
                let metadata = if let Some(parent_id) = previous.clone() {
                    manager
                        .continue_video(ContinueVideoRequest {
                            parent_local_id: parent_id,
                            local_id: clip_local_id.clone(),
                            prompt,
                            model: model.clone(),
                            size: size.clone(),
                            seconds,
                        })
                        .await?
                } else {
                    manager
                        .create_video(CreateVideoRequest {
                            local_id: clip_local_id.clone(),
                            prompt,
                            model: model.clone(),
                            size: size.clone(),
                            seconds,
                        })
                        .await?
                };

                print_metadata(&metadata);
                previous = Some(metadata.local_id.clone());
                generated_ids.push(metadata.local_id);
            }

            let mut clips_for_stitch = Vec::new();
            if let Some(start) = start_clip {
                clips_for_stitch.push(start);
            }
            clips_for_stitch.extend(generated_ids);

            let stitched_path = manager
                .stitch_videos(&id, &clips_for_stitch)
                .await
                .context("failed to stitch flow clips")?;

            println!("flow stitched {} -> {}", id, stitched_path.display());
        }
        Command::List => {
            let videos = manager.list_videos().await?;
            if videos.is_empty() {
                println!("(no clips recorded)");
            } else {
                for video in videos {
                    print_metadata(&video);
                }
            }
        }
        Command::Download {
            id,
            variant,
            output,
        } => {
            let variant = match variant {
                AssetVariant::Video => VideoVariant::Video,
                AssetVariant::Thumbnail => VideoVariant::Thumbnail,
                AssetVariant::Spritesheet => VideoVariant::Spritesheet,
            };

            manager
                .download_asset(&id, variant, &output)
                .await
                .context("failed to download asset")?;

            info!(path = %output.display(), "downloaded asset");
        }
        Command::Stitch { id, clips } => {
            let path = manager
                .stitch_videos(&id, &clips)
                .await
                .context("failed to stitch clips")?;

            println!("stitched {} -> {}", id, path.display());
        }
    }

    Ok(())
}

fn setup_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .try_init();
}

fn print_metadata(metadata: &continuator::VideoMetadata) {
    println!("id: {}", metadata.local_id);
    println!("remote_id: {}", metadata.remote_id);
    println!("backend: {:?}", metadata.backend);
    println!("model: {}", metadata.model);
    println!("seconds: {}", metadata.seconds);
    println!("size: {}", metadata.size);
    if let Some(parent) = &metadata.parent {
        println!("parent: {}", parent);
    }
    if let Some(created_at) = metadata.created_at {
        println!("created_at: {}", created_at);
    }
    println!("file: {}", metadata.file_path.display());
    println!("prompt: {}", metadata.prompt);
    println!();
}
