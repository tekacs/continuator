use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use sora_continuator::{
    ContinueVideoRequest, CreateVideoRequest, SoraConfig, VideoManager, VideoVariant,
};
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(author, version, about = "Generate and extend Sora videos via the CLI", long_about = None)]
struct Cli {
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

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Create a brand-new Sora clip.
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
        api_key: cli.api_key,
        model: cli.model,
        size: cli.size,
        seconds: cli.seconds,
        data_dir: cli.data_dir,
        poll_interval_ms: cli.poll_interval_ms,
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

fn print_metadata(metadata: &sora_continuator::VideoMetadata) {
    println!("id: {}", metadata.local_id);
    println!("remote_id: {}", metadata.remote_id);
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
