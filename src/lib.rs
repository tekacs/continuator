use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use clap::ValueEnum;
use futures_util::StreamExt;
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, process::Command, time::sleep};
use tracing::debug;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_SORA_MODEL: &str = "sora-2";
const DEFAULT_SECONDS: u32 = 12;
const DEFAULT_SIZE: &str = "1280x720";
const DEFAULT_POLL_INTERVAL_MS: u64 = 5_000;
const THUMBNAIL_VARIANT: &str = "thumbnail";
const SPRITESHEET_VARIANT: &str = "spritesheet";

const DEFAULT_VEO_MODEL: &str = "veo-3.0-generate-preview";
const DEFAULT_VEO_SECONDS: u32 = 8;

/// Error type for all operations in this crate.
#[derive(Debug, Error)]
pub enum SoraError {
    #[error("missing OPENAI_API_KEY environment variable")]
    MissingApiKey,
    #[error("missing Google Cloud project id for Veo")]
    MissingGcpProject,
    #[error("missing Google Cloud location for Veo")]
    MissingGcpLocation,
    #[error("unable to obtain Google Cloud access token")]
    MissingGcpToken,
    #[error("Google Cloud auth failed: {0}")]
    GcpAuth(String),
    #[error("HTTP request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON serialization error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("ffmpeg not found on PATH")]
    FfmpegMissing,
    #[error("ffmpeg command failed: {0}")]
    FfmpegFailed(String),
    #[error("video concatenation failed: {0}")]
    FfmpegConcatFailed(String),
    #[error("video generation job failed: {0}")]
    JobFailed(String),
    #[error("video not found locally: {0}")]
    VideoNotFound(String),
    #[error("metadata missing for video: {0}")]
    MetadataNotFound(String),
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),
    #[error("operation unsupported for backend: {0}")]
    UnsupportedOperation(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Sora,
    Veo,
}

impl ProviderKind {
    const fn default_backend() -> Self {
        ProviderKind::Sora
    }
}

/// High-level configuration for the video pipeline.
#[derive(Debug, Clone)]
pub struct ContinuatorConfig {
    /// Backend provider to target (defaults to Sora).
    pub provider: Option<ProviderKind>,
    /// API key for OpenAI Sora.
    pub api_key: Option<String>,
    /// Preferred model identifier.
    pub model: Option<String>,
    /// Dimensions string (e.g., `1280x720`).
    pub size: Option<String>,
    /// Length of each clip in seconds.
    pub seconds: Option<u32>,
    /// Root directory for downloaded videos and metadata.
    pub data_dir: Option<PathBuf>,
    /// Polling interval in milliseconds.
    pub poll_interval_ms: Option<u64>,
    /// Google Cloud project id for Veo.
    pub gcp_project: Option<String>,
    /// Google Cloud location for Veo.
    pub gcp_location: Option<String>,
    /// Pre-fetched OAuth access token for Veo requests.
    pub gcp_access_token: Option<String>,
    /// Optional Cloud Storage URI to store generated videos.
    pub gcp_storage_uri: Option<String>,
    /// Whether to request audio generation for Veo (defaults to true).
    pub gcp_generate_audio: Option<bool>,
    /// Preferred Veo resolution ("720p" or "1080p").
    pub gcp_resolution: Option<String>,
    /// Whether to let Gemini enhance prompts for Veo (defaults to true).
    pub gcp_enhance_prompt: Option<bool>,
}

impl Default for ContinuatorConfig {
    fn default() -> Self {
        Self {
            provider: None,
            api_key: None,
            model: None,
            size: None,
            seconds: None,
            data_dir: None,
            poll_interval_ms: None,
            gcp_project: None,
            gcp_location: None,
            gcp_access_token: None,
            gcp_storage_uri: None,
            gcp_generate_audio: None,
            gcp_resolution: None,
            gcp_enhance_prompt: None,
        }
    }
}

pub type SoraConfig = ContinuatorConfig;

impl ContinuatorConfig {
    fn resolve(&self) -> Result<ResolvedManagerConfig, SoraError> {
        let provider = self.provider.unwrap_or(ProviderKind::Sora);
        let data_dir = self
            .data_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("videos"));
        let poll_interval =
            Duration::from_millis(self.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS));

        let backend = match provider {
            ProviderKind::Sora => {
                let api_key = match self.api_key.clone() {
                    Some(key) => key,
                    None => {
                        std::env::var("OPENAI_API_KEY").map_err(|_| SoraError::MissingApiKey)?
                    }
                };
                let defaults = BackendDefaults {
                    model: self
                        .model
                        .clone()
                        .unwrap_or_else(|| DEFAULT_SORA_MODEL.to_string()),
                    size: self
                        .size
                        .clone()
                        .unwrap_or_else(|| DEFAULT_SIZE.to_string()),
                    seconds: self.seconds.unwrap_or(DEFAULT_SECONDS),
                };
                let client = SoraClient::new(api_key.clone())?;
                Backend::Sora(SoraBackend { client, defaults })
            }
            ProviderKind::Veo => {
                let project = self
                    .gcp_project
                    .clone()
                    .ok_or(SoraError::MissingGcpProject)?;
                let location = self
                    .gcp_location
                    .clone()
                    .ok_or(SoraError::MissingGcpLocation)?;
                let token_source = if let Some(token) = self.gcp_access_token.clone() {
                    VeoTokenSource::Static(token)
                } else {
                    VeoTokenSource::Gcloud
                };
                let defaults = BackendDefaults {
                    model: self
                        .model
                        .clone()
                        .unwrap_or_else(|| DEFAULT_VEO_MODEL.to_string()),
                    size: self
                        .size
                        .clone()
                        .unwrap_or_else(|| DEFAULT_SIZE.to_string()),
                    seconds: self.seconds.unwrap_or(DEFAULT_VEO_SECONDS),
                };
                let generate_audio = self.gcp_generate_audio.unwrap_or(true);
                let enhance_prompt = self.gcp_enhance_prompt.unwrap_or(true);
                let resolution = self.gcp_resolution.clone();
                let aspect_ratio = size_to_aspect_ratio(defaults.size.as_str());
                let client = VeoClient::new(project, location, token_source)?;
                Backend::Veo(VeoBackend {
                    client,
                    defaults,
                    generate_audio,
                    enhance_prompt,
                    storage_uri: self.gcp_storage_uri.clone(),
                    resolution,
                    aspect_ratio,
                })
            }
        };

        Ok(ResolvedManagerConfig {
            backend,
            data_dir,
            poll_interval,
        })
    }
}

struct ResolvedManagerConfig {
    backend: Backend,
    data_dir: PathBuf,
    poll_interval: Duration,
}

#[derive(Debug)]
struct BackendDefaults {
    model: String,
    size: String,
    seconds: u32,
}

#[derive(Debug)]
enum Backend {
    Sora(SoraBackend),
    Veo(VeoBackend),
}

impl Backend {
    fn kind(&self) -> ProviderKind {
        match self {
            Backend::Sora(_) => ProviderKind::Sora,
            Backend::Veo(_) => ProviderKind::Veo,
        }
    }

    fn defaults(&self) -> &BackendDefaults {
        match self {
            Backend::Sora(backend) => &backend.defaults,
            Backend::Veo(backend) => &backend.defaults,
        }
    }

    async fn render(&self, ctx: RenderContext<'_>) -> Result<RenderOutcome, SoraError> {
        match self {
            Backend::Sora(backend) => backend.render(ctx).await,
            Backend::Veo(backend) => backend.render(ctx).await,
        }
    }

    async fn download(
        &self,
        remote_id: &str,
        variant: VideoVariant,
        output_path: &Path,
    ) -> Result<(), SoraError> {
        match self {
            Backend::Sora(backend) => backend.download(remote_id, variant, output_path).await,
            Backend::Veo(backend) => backend.download(remote_id, variant, output_path).await,
        }
    }
}

struct RenderContext<'a> {
    prompt: &'a str,
    model: &'a str,
    seconds: u32,
    size: &'a str,
    poll_interval: Duration,
    output_path: &'a Path,
    first_frame_path: Option<&'a Path>,
}

struct RenderOutcome {
    remote_id: String,
    model: String,
    seconds: u32,
    size: String,
    created_at: Option<i64>,
}

#[derive(Debug)]
struct SoraBackend {
    client: SoraClient,
    defaults: BackendDefaults,
}

impl SoraBackend {
    async fn render(&self, ctx: RenderContext<'_>) -> Result<RenderOutcome, SoraError> {
        let mut builder = ApiCreateRequest {
            prompt: ctx.prompt.to_string(),
            model: ctx.model.to_string(),
            seconds: ctx.seconds,
            size: ctx.size.to_string(),
            input_reference_path: ctx.first_frame_path.map(|path| path.to_path_buf()),
        };

        let job = self.client.create_video(&mut builder).await?;
        let job = self
            .wait_for_completion(job.id.clone(), ctx.poll_interval)
            .await?;

        self.client
            .download_video(&job.id, VideoVariant::Video, ctx.output_path)
            .await?;

        Ok(RenderOutcome {
            remote_id: job.id,
            model: job.model,
            seconds: job.seconds.unwrap_or(ctx.seconds),
            size: job.size.unwrap_or_else(|| ctx.size.to_string()),
            created_at: job.created_at,
        })
    }

    async fn download(
        &self,
        remote_id: &str,
        variant: VideoVariant,
        output_path: &Path,
    ) -> Result<(), SoraError> {
        self.client
            .download_video(remote_id, variant, output_path)
            .await
    }

    async fn wait_for_completion(
        &self,
        remote_id: String,
        poll_interval: Duration,
    ) -> Result<VideoJob, SoraError> {
        loop {
            let job = self.client.retrieve_video(&remote_id).await?;
            match job.status {
                VideoStatus::Completed => return Ok(job),
                VideoStatus::Failed => {
                    let message = job
                        .error
                        .and_then(|e| e.message)
                        .unwrap_or_else(|| "unknown error".to_string());
                    return Err(SoraError::JobFailed(message));
                }
                VideoStatus::Canceled => {
                    return Err(SoraError::JobFailed("job was canceled".to_string()));
                }
                _ => {
                    debug!(id = remote_id, status = ?job.status, "polling video status");
                    sleep(poll_interval).await;
                }
            }
        }
    }
}

#[derive(Debug)]
struct VeoBackend {
    client: VeoClient,
    defaults: BackendDefaults,
    generate_audio: bool,
    enhance_prompt: bool,
    storage_uri: Option<String>,
    resolution: Option<String>,
    aspect_ratio: Option<String>,
}

impl VeoBackend {
    async fn render(&self, ctx: RenderContext<'_>) -> Result<RenderOutcome, SoraError> {
        validate_veo_duration(ctx.seconds)?;
        let resolution = self
            .resolution
            .clone()
            .or_else(|| size_to_resolution(ctx.size));
        let aspect_ratio = self
            .aspect_ratio
            .clone()
            .or_else(|| size_to_aspect_ratio(ctx.size));

        let image_base64 = if let Some(path) = ctx.first_frame_path {
            Some(encode_first_frame(path).await?)
        } else {
            None
        };
        let image = image_base64.as_ref().map(|data| VeoImage {
            bytes_base64_encoded: Some(data.clone()),
            gcs_uri: None,
            mime_type: "image/png".to_string(),
        });

        let payload = VeoPredictRequest {
            instances: vec![VeoInstance {
                prompt: ctx.prompt,
                image,
            }],
            parameters: VeoParameters {
                duration_seconds: ctx.seconds,
                generate_audio: self.generate_audio,
                storage_uri: self.storage_uri.as_deref(),
                resolution: resolution.as_deref(),
                aspect_ratio: aspect_ratio.as_deref(),
                enhance_prompt: self.enhance_prompt,
                sample_count: None,
            },
        };

        let operation = self.client.submit_job(ctx.model, payload).await?;

        let response = self
            .client
            .poll_operation(ctx.model, &operation, ctx.poll_interval)
            .await?;

        let videos = response.videos;
        let maybe_bytes = videos
            .iter()
            .find_map(|video| video.bytes_base64_encoded.clone());
        let video_bytes = if let Some(bytes) = maybe_bytes {
            bytes
        } else if videos.iter().any(|video| video.gcs_uri.is_some()) {
            return Err(SoraError::UnsupportedOperation(
                "Veo returned Cloud Storage URIs; provide gcp_storage_uri= or download manually"
                    .to_string(),
            ));
        } else {
            return Err(SoraError::InvalidResponse(
                "Veo response missing video payload".to_string(),
            ));
        };

        let data = BASE64_STANDARD.decode(video_bytes).map_err(|err| {
            SoraError::InvalidResponse(format!("invalid base64 video payload: {err}"))
        })?;

        fs::write(ctx.output_path, data).await?;

        Ok(RenderOutcome {
            remote_id: operation,
            model: ctx.model.to_string(),
            seconds: ctx.seconds,
            size: ctx.size.to_string(),
            created_at: None,
        })
    }

    async fn download(
        &self,
        _remote_id: &str,
        variant: VideoVariant,
        _output_path: &Path,
    ) -> Result<(), SoraError> {
        Err(SoraError::UnsupportedOperation(format!(
            "Veo backend does not support downloading {variant:?} directly"
        )))
    }
}

fn validate_veo_duration(seconds: u32) -> Result<(), SoraError> {
    match seconds {
        4 | 6 | 8 => Ok(()),
        other => Err(SoraError::InvalidConfig(format!(
            "Veo 3 Preview requires duration 4, 6, or 8 seconds (got {other})"
        ))),
    }
}

fn size_to_resolution(size: &str) -> Option<String> {
    match size {
        "1280x720" | "720x1280" => Some("720p".to_string()),
        "1920x1080" | "1080x1920" => Some("1080p".to_string()),
        _ => None,
    }
}

fn size_to_aspect_ratio(size: &str) -> Option<String> {
    match size {
        "1280x720" | "1920x1080" => Some("16:9".to_string()),
        "720x1280" | "1080x1920" => Some("9:16".to_string()),
        _ => None,
    }
}

async fn encode_first_frame(path: &Path) -> Result<String, SoraError> {
    let bytes = fs::read(path).await?;
    Ok(BASE64_STANDARD.encode(bytes))
}

/// Request for creating a brand-new video.
#[derive(Debug, Clone)]
pub struct CreateVideoRequest {
    pub prompt: String,
    /// Local identifier for saving the video; used as filename stem.
    pub local_id: String,
    pub model: Option<String>,
    pub seconds: Option<u32>,
    pub size: Option<String>,
}

/// Request for creating a continuation using the last frame of an existing video.
#[derive(Debug, Clone)]
pub struct ContinueVideoRequest {
    /// Existing local video identifier to continue from.
    pub parent_local_id: String,
    /// New local identifier for the continuation clip.
    pub local_id: String,
    pub prompt: String,
    pub model: Option<String>,
    pub seconds: Option<u32>,
    pub size: Option<String>,
}

/// Stored metadata for each downloaded clip.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub local_id: String,
    pub remote_id: String,
    pub prompt: String,
    pub model: String,
    pub seconds: u32,
    pub size: String,
    pub created_at: Option<i64>,
    pub file_path: PathBuf,
    pub parent: Option<String>,
    #[serde(default = "ProviderKind::default_backend")]
    pub backend: ProviderKind,
}

/// Primary entry point for managing videos and continuations.
pub struct VideoManager {
    backend: Backend,
    data_dir: PathBuf,
    poll_interval: Duration,
}

impl VideoManager {
    /// Build a new manager from high-level configuration.
    pub fn new(config: ContinuatorConfig) -> Result<Self, SoraError> {
        let resolved = config.resolve()?;
        Ok(Self {
            backend: resolved.backend,
            data_dir: resolved.data_dir,
            poll_interval: resolved.poll_interval,
        })
    }

    /// Ensure the data directory exists on disk.
    async fn ensure_data_dir(&self) -> Result<(), SoraError> {
        fs::create_dir_all(&self.data_dir).await?;
        Ok(())
    }

    fn video_path(&self, local_id: &str) -> PathBuf {
        self.data_dir.join(format!("{local_id}.mp4"))
    }

    fn metadata_path(&self, local_id: &str) -> PathBuf {
        self.data_dir.join(format!("{local_id}.json"))
    }

    async fn save_metadata(&self, metadata: &VideoMetadata) -> Result<(), SoraError> {
        let path = self.metadata_path(&metadata.local_id);
        let data = serde_json::to_vec_pretty(metadata)?;
        fs::write(path, data).await?;
        Ok(())
    }

    async fn load_metadata(&self, local_id: &str) -> Result<VideoMetadata, SoraError> {
        let path = self.metadata_path(local_id);
        let bytes = fs::read(&path)
            .await
            .map_err(|_| SoraError::MetadataNotFound(local_id.to_string()))?;
        let metadata: VideoMetadata = serde_json::from_slice(&bytes)?;
        Ok(metadata)
    }

    /// Fetch the metadata for a given local identifier.
    pub async fn get_metadata(&self, local_id: &str) -> Result<VideoMetadata, SoraError> {
        self.load_metadata(local_id).await
    }

    /// Download a variant of the rendered asset (video, thumbnail, spritesheet).
    pub async fn download_asset(
        &self,
        local_id: &str,
        variant: VideoVariant,
        output_path: &Path,
    ) -> Result<(), SoraError> {
        let metadata = self.load_metadata(local_id).await?;
        if let Some(parent) = output_path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).await?;
            }
        }

        if metadata.backend == ProviderKind::Veo && matches!(variant, VideoVariant::Video) {
            fs::copy(&metadata.file_path, output_path).await?;
            return Ok(());
        }

        self.backend
            .download(&metadata.remote_id, variant, output_path)
            .await
    }

    /// Generate a brand-new clip using the configured backend and persist the results locally.
    pub async fn create_video(
        &self,
        request: CreateVideoRequest,
    ) -> Result<VideoMetadata, SoraError> {
        self.ensure_data_dir().await?;
        if fs::try_exists(self.metadata_path(&request.local_id)).await? {
            return Err(SoraError::InvalidConfig(format!(
                "local id '{}' already exists",
                request.local_id
            )));
        }

        let defaults = self.backend.defaults();
        let model = request
            .model
            .as_deref()
            .unwrap_or(&defaults.model)
            .to_string();
        let size = request
            .size
            .as_deref()
            .unwrap_or(&defaults.size)
            .to_string();
        let seconds = request.seconds.unwrap_or(defaults.seconds);

        let video_path = self.video_path(&request.local_id);
        let outcome = self
            .backend
            .render(RenderContext {
                prompt: &request.prompt,
                model: &model,
                seconds,
                size: &size,
                poll_interval: self.poll_interval,
                output_path: &video_path,
                first_frame_path: None,
            })
            .await?;

        let metadata = VideoMetadata {
            local_id: request.local_id,
            remote_id: outcome.remote_id,
            prompt: request.prompt,
            model: outcome.model,
            seconds: outcome.seconds,
            size: outcome.size,
            created_at: outcome.created_at,
            file_path: video_path,
            parent: None,
            backend: self.backend.kind(),
        };

        self.save_metadata(&metadata).await?;
        Ok(metadata)
    }

    /// Create a continuation using the last frame of an existing clip as an image reference.
    pub async fn continue_video(
        &self,
        request: ContinueVideoRequest,
    ) -> Result<VideoMetadata, SoraError> {
        self.ensure_data_dir().await?;
        if fs::try_exists(self.metadata_path(&request.local_id)).await? {
            return Err(SoraError::InvalidConfig(format!(
                "local id '{}' already exists",
                request.local_id
            )));
        }
        let parent = self.load_metadata(&request.parent_local_id).await?;
        let parent_video_path = self.video_path(&request.parent_local_id);

        if !parent_video_path.exists() {
            return Err(SoraError::VideoNotFound(request.parent_local_id));
        }

        let last_frame_path = self
            .extract_last_frame(&parent_video_path, &request.local_id)
            .await?;

        let defaults = self.backend.defaults();
        let model = request
            .model
            .as_ref()
            .or_else(|| Some(&parent.model))
            .unwrap_or(&defaults.model)
            .to_string();
        let size = request
            .size
            .as_ref()
            .or_else(|| Some(&parent.size))
            .unwrap_or(&defaults.size)
            .to_string();
        let seconds = request
            .seconds
            .or(Some(parent.seconds))
            .unwrap_or(defaults.seconds);

        let video_path = self.video_path(&request.local_id);
        let outcome = self
            .backend
            .render(RenderContext {
                prompt: &request.prompt,
                model: &model,
                seconds,
                size: &size,
                poll_interval: self.poll_interval,
                output_path: &video_path,
                first_frame_path: Some(&last_frame_path),
            })
            .await?;

        let metadata = VideoMetadata {
            local_id: request.local_id,
            remote_id: outcome.remote_id,
            prompt: request.prompt,
            model: outcome.model,
            seconds: outcome.seconds,
            size: outcome.size,
            created_at: outcome.created_at,
            file_path: video_path,
            parent: Some(parent.local_id),
            backend: self.backend.kind(),
        };

        self.save_metadata(&metadata).await?;

        let _ = fs::remove_file(last_frame_path).await;

        Ok(metadata)
    }

    /// Enumerate all locally stored clips.
    pub async fn list_videos(&self) -> Result<Vec<VideoMetadata>, SoraError> {
        self.ensure_data_dir().await?;
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&self.data_dir).await?;
        while let Some(entry) = dir.next_entry().await? {
            if entry.path().extension().and_then(|s| s.to_str()) == Some("json") {
                let stem = entry
                    .path()
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string());
                if let Some(local_id) = stem {
                    if let Ok(metadata) = self.load_metadata(&local_id).await {
                        entries.push(metadata);
                    }
                }
            }
        }
        entries.sort_by(|a, b| a.local_id.cmp(&b.local_id));
        Ok(entries)
    }

    /// Concatenate multiple local clips into a single MP4 under the output identifier.
    pub async fn stitch_videos(
        &self,
        output_local_id: &str,
        input_local_ids: &[String],
    ) -> Result<PathBuf, SoraError> {
        if input_local_ids.is_empty() {
            return Err(SoraError::InvalidConfig(
                "stitch requires at least one input clip".to_string(),
            ));
        }

        self.ensure_data_dir().await?;

        let output_path = self.video_path(output_local_id);
        let manifest_path = self
            .data_dir
            .join(format!(".concat-{}.txt", output_local_id));

        let mut manifest = String::new();
        for id in input_local_ids {
            let metadata = self.load_metadata(id).await?;
            if !metadata.file_path.exists() {
                return Err(SoraError::VideoNotFound(id.clone()));
            }
            let abs_path = fs::canonicalize(&metadata.file_path).await?;
            manifest.push_str(&format!("file '{}'\n", abs_path.display()));
        }

        fs::write(&manifest_path, manifest).await?;

        let status = Command::new("ffmpeg")
            .arg("-y")
            .arg("-f")
            .arg("concat")
            .arg("-safe")
            .arg("0")
            .arg("-i")
            .arg(&manifest_path)
            .arg("-c")
            .arg("copy")
            .arg(&output_path)
            .status()
            .await
            .map_err(|_| SoraError::FfmpegMissing)?;

        let _ = fs::remove_file(&manifest_path).await;

        if !status.success() {
            return Err(SoraError::FfmpegConcatFailed(format!(
                "ffmpeg exited with status {status}"
            )));
        }

        Ok(output_path)
    }

    async fn extract_last_frame(
        &self,
        video_path: &Path,
        local_id: &str,
    ) -> Result<PathBuf, SoraError> {
        let frame_path = std::env::temp_dir().join(format!("{local_id}_last.png"));
        let status = Command::new("ffmpeg")
            .arg("-v")
            .arg("error")
            .arg("-i")
            .arg(video_path)
            .arg("-vf")
            .arg("reverse")
            .arg("-frames:v")
            .arg("1")
            .arg("-y")
            .arg(&frame_path)
            .status()
            .await
            .map_err(|_| SoraError::FfmpegMissing)?;

        if !status.success() {
            return Err(SoraError::FfmpegFailed(format!(
                "ffmpeg exited with status {status}"
            )));
        }

        Ok(frame_path)
    }
}

/// Type-safe variants for downloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoVariant {
    Video,
    Thumbnail,
    Spritesheet,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VideoJob {
    pub id: String,
    pub object: Option<String>,
    pub created_at: Option<i64>,
    pub status: VideoStatus,
    pub model: String,
    pub progress: Option<f64>,
    #[serde(deserialize_with = "deserialize_optional_u32", default)]
    pub seconds: Option<u32>,
    pub size: Option<String>,
    pub error: Option<ApiError>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoStatus {
    Queued,
    InProgress,
    Completed,
    Failed,
    Canceled,
    Unknown(String),
}

fn deserialize_optional_u32<'de, D>(deserializer: D) -> Result<Option<u32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let opt = Option::<serde_json::Value>::deserialize(deserializer)?;
    if let Some(value) = opt {
        match value {
            serde_json::Value::Number(n) => n
                .as_u64()
                .map(|n| n as u32)
                .ok_or_else(|| serde::de::Error::custom("seconds field is not a valid u32"))
                .map(Some),
            serde_json::Value::String(s) => s
                .parse::<u32>()
                .map(Some)
                .map_err(|_| serde::de::Error::custom("seconds field string is not a valid u32")),
            _ => Err(serde::de::Error::custom(
                "unexpected type for seconds field",
            )),
        }
    } else {
        Ok(None)
    }
}

impl<'de> Deserialize<'de> for VideoStatus {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let status = match value.as_str() {
            "queued" => VideoStatus::Queued,
            "in_progress" => VideoStatus::InProgress,
            "completed" => VideoStatus::Completed,
            "failed" => VideoStatus::Failed,
            "canceled" => VideoStatus::Canceled,
            other => VideoStatus::Unknown(other.to_string()),
        };
        Ok(status)
    }
}

impl Serialize for VideoStatus {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let value = match self {
            VideoStatus::Queued => "queued",
            VideoStatus::InProgress => "in_progress",
            VideoStatus::Completed => "completed",
            VideoStatus::Failed => "failed",
            VideoStatus::Canceled => "canceled",
            VideoStatus::Unknown(other) => other.as_str(),
        };
        serializer.serialize_str(value)
    }
}

impl VideoStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            VideoStatus::Completed | VideoStatus::Failed | VideoStatus::Canceled
        )
    }
}

struct ApiCreateRequest {
    prompt: String,
    model: String,
    seconds: u32,
    size: String,
    input_reference_path: Option<PathBuf>,
}

impl ApiCreateRequest {
    fn build_form(&self) -> Result<multipart::Form, SoraError> {
        let mut form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("prompt", self.prompt.clone())
            .text("seconds", self.seconds.to_string())
            .text("size", self.size.clone());

        if let Some(path) = &self.input_reference_path {
            let data = std::fs::read(path)?;
            let part = multipart::Part::bytes(data)
                .file_name("input.png")
                .mime_str("image/png")
                .map_err(SoraError::Request)?;
            form = form.part("input_reference", part);
        }

        Ok(form)
    }
}

#[derive(Debug, Clone)]
struct SoraClient {
    http: reqwest::Client,
    api_key: String,
}

impl SoraClient {
    fn new(api_key: String) -> Result<Self, SoraError> {
        let http = reqwest::Client::builder().build()?;
        Ok(Self { http, api_key })
    }

    async fn create_video(&self, request: &mut ApiCreateRequest) -> Result<VideoJob, SoraError> {
        let form = request.build_form()?;
        let url = format!("{OPENAI_API_BASE}/videos");
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await?;

        Self::handle_response(response).await
    }

    async fn retrieve_video(&self, video_id: &str) -> Result<VideoJob, SoraError> {
        let url = format!("{OPENAI_API_BASE}/videos/{video_id}");
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;
        Self::handle_response(response).await
    }

    async fn download_video(
        &self,
        video_id: &str,
        variant: VideoVariant,
        path: &Path,
    ) -> Result<(), SoraError> {
        let mut url = format!("{OPENAI_API_BASE}/videos/{video_id}/content");
        match variant {
            VideoVariant::Video => {}
            VideoVariant::Thumbnail => {
                url.push_str(&format!("?variant={THUMBNAIL_VARIANT}"));
            }
            VideoVariant::Spritesheet => {
                url.push_str(&format!("?variant={SPRITESHEET_VARIANT}"));
            }
        }

        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(SoraError::Request(response.error_for_status().unwrap_err()));
        }

        let mut file = fs::File::create(path).await?;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok(())
    }

    async fn handle_response(response: reqwest::Response) -> Result<VideoJob, SoraError> {
        let status = response.status();
        if status == StatusCode::NO_CONTENT {
            return Err(SoraError::InvalidConfig(
                "empty response from server".to_string(),
            ));
        }

        if !status.is_success() {
            let text = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(SoraError::JobFailed(format!(
                "API error ({status}): {text}"
            )));
        }

        let job = response.json::<VideoJob>().await?;
        Ok(job)
    }
}

#[derive(Debug, Clone)]
struct VeoClient {
    http: reqwest::Client,
    project: String,
    location: String,
    token_source: VeoTokenSource,
}

impl VeoClient {
    fn new(
        project: String,
        location: String,
        token_source: VeoTokenSource,
    ) -> Result<Self, SoraError> {
        let http = reqwest::Client::builder().build()?;
        Ok(Self {
            http,
            project,
            location,
            token_source,
        })
    }

    async fn submit_job(
        &self,
        model_id: &str,
        payload: VeoPredictRequest<'_>,
    ) -> Result<String, SoraError> {
        let token = self.token_source.access_token().await?;
        let url = format!(
            "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:predictLongRunning",
            self.location, self.project, self.location, model_id
        );
        let response = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(&payload)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(SoraError::JobFailed(format!(
                "Veo predictLongRunning failed ({status}): {body}"
            )));
        }

        let envelope: VeoOperationName = response.json().await?;
        Ok(envelope.name)
    }

    async fn poll_operation(
        &self,
        model_id: &str,
        operation_name: &str,
        poll_interval: Duration,
    ) -> Result<VeoOperationResponse, SoraError> {
        loop {
            let token = self.token_source.access_token().await?;
            let url = format!(
                "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers/google/models/{}:fetchPredictOperation",
                self.location, self.project, self.location, model_id
            );
            let body = VeoFetchRequest {
                operation_name: operation_name.to_string(),
            };
            let response = self
                .http
                .post(&url)
                .bearer_auth(&token)
                .json(&body)
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let text = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "<no body>".to_string());
                return Err(SoraError::JobFailed(format!(
                    "Veo fetchPredictOperation failed ({status}): {text}"
                )));
            }

            let status: VeoFetchResponse = response.json().await?;
            if let Some(error) = status.error {
                let message = error.message.unwrap_or_else(|| "unknown error".to_string());
                return Err(SoraError::JobFailed(message));
            }
            if status.done.unwrap_or(false) {
                if let Some(response) = status.response {
                    return Ok(response);
                }
                return Err(SoraError::InvalidResponse(
                    "operation completed without response payload".to_string(),
                ));
            }

            sleep(poll_interval).await;
        }
    }
}

#[derive(Debug, Clone)]
enum VeoTokenSource {
    Static(String),
    Gcloud,
}

impl VeoTokenSource {
    async fn access_token(&self) -> Result<String, SoraError> {
        match self {
            VeoTokenSource::Static(token) => Ok(token.clone()),
            VeoTokenSource::Gcloud => {
                let output = Command::new("gcloud")
                    .arg("auth")
                    .arg("print-access-token")
                    .output()
                    .await
                    .map_err(|err| {
                        if err.kind() == std::io::ErrorKind::NotFound {
                            SoraError::MissingGcpToken
                        } else {
                            SoraError::GcpAuth(err.to_string())
                        }
                    })?;
                if !output.status.success() {
                    return Err(SoraError::GcpAuth(format!(
                        "gcloud exited with status {}",
                        output.status
                    )));
                }
                let token = String::from_utf8(output.stdout)
                    .map_err(|err| SoraError::GcpAuth(err.to_string()))?
                    .trim()
                    .to_string();
                if token.is_empty() {
                    return Err(SoraError::MissingGcpToken);
                }
                Ok(token)
            }
        }
    }
}

#[derive(Serialize)]
struct VeoImage {
    #[serde(rename = "bytesBase64Encoded", skip_serializing_if = "Option::is_none")]
    bytes_base64_encoded: Option<String>,
    #[serde(rename = "gcsUri", skip_serializing_if = "Option::is_none")]
    gcs_uri: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: String,
}

#[derive(Serialize)]
struct VeoInstance<'a> {
    prompt: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<VeoImage>,
}

#[derive(Serialize)]
struct VeoParameters<'a> {
    #[serde(rename = "durationSeconds")]
    duration_seconds: u32,
    #[serde(rename = "generateAudio")]
    generate_audio: bool,
    #[serde(rename = "storageUri", skip_serializing_if = "Option::is_none")]
    storage_uri: Option<&'a str>,
    #[serde(rename = "resolution", skip_serializing_if = "Option::is_none")]
    resolution: Option<&'a str>,
    #[serde(rename = "aspectRatio", skip_serializing_if = "Option::is_none")]
    aspect_ratio: Option<&'a str>,
    #[serde(rename = "enhancePrompt")]
    enhance_prompt: bool,
    #[serde(rename = "sampleCount", skip_serializing_if = "Option::is_none")]
    sample_count: Option<u32>,
}

#[derive(Serialize)]
struct VeoPredictRequest<'a> {
    instances: Vec<VeoInstance<'a>>,
    parameters: VeoParameters<'a>,
}

#[derive(Deserialize)]
struct VeoOperationName {
    name: String,
}

#[derive(Serialize)]
struct VeoFetchRequest {
    #[serde(rename = "operationName")]
    operation_name: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct VeoFetchResponse {
    name: String,
    done: Option<bool>,
    response: Option<VeoOperationResponse>,
    error: Option<VeoOperationError>,
}

#[derive(Debug, Deserialize)]
struct VeoOperationError {
    message: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct VeoOperationResponse {
    #[serde(rename = "@type")]
    type_url: Option<String>,
    #[serde(default)]
    videos: Vec<VeoGeneratedVideo>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Clone)]
struct VeoGeneratedVideo {
    #[serde(rename = "gcsUri")]
    gcs_uri: Option<String>,
    #[serde(rename = "bytesBase64Encoded")]
    bytes_base64_encoded: Option<String>,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_deserialization() {
        let json = "\"completed\"";
        let status: VideoStatus = serde_json::from_str(json).unwrap();
        assert!(matches!(status, VideoStatus::Completed));

        let json = "\"mysterious\"";
        let status: VideoStatus = serde_json::from_str(json).unwrap();
        assert!(matches!(status, VideoStatus::Unknown(_)));
    }
}
