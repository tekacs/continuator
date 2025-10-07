use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use futures_util::StreamExt;
use reqwest::{StatusCode, multipart};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{fs, io::AsyncWriteExt, process::Command, time::sleep};
use tracing::debug;

const OPENAI_API_BASE: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "sora-2";
const DEFAULT_SECONDS: u32 = 12;
const DEFAULT_SIZE: &str = "1280x720";
const DEFAULT_POLL_INTERVAL_MS: u64 = 5_000;
const THUMBNAIL_VARIANT: &str = "thumbnail";
const SPRITESHEET_VARIANT: &str = "spritesheet";

/// Error type for all operations in this crate.
#[derive(Debug, Error)]
pub enum SoraError {
    #[error("missing OPENAI_API_KEY environment variable")]
    MissingApiKey,
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
}

/// High-level configuration for the Sora pipeline.
#[derive(Debug, Clone)]
pub struct SoraConfig {
    /// API key; defaults to `OPENAI_API_KEY`.
    pub api_key: Option<String>,
    /// Preferred Sora model. Defaults to `sora-2`.
    pub model: Option<String>,
    /// Dimensions string (e.g., `1280x720`). Defaults to 720p.
    pub size: Option<String>,
    /// Length of each clip in seconds. Defaults to 10 seconds.
    pub seconds: Option<u32>,
    /// Root directory for downloaded videos and metadata. Defaults to `videos/`.
    pub data_dir: Option<PathBuf>,
    /// Polling interval in milliseconds. Defaults to 5000.
    pub poll_interval_ms: Option<u64>,
}

impl Default for SoraConfig {
    fn default() -> Self {
        Self {
            api_key: None,
            model: None,
            size: None,
            seconds: None,
            data_dir: None,
            poll_interval_ms: None,
        }
    }
}

impl SoraConfig {
    fn resolve(&self) -> Result<ResolvedConfig, SoraError> {
        let api_key = match self.api_key.clone() {
            Some(key) => key,
            None => std::env::var("OPENAI_API_KEY").map_err(|_| SoraError::MissingApiKey)?,
        };

        let data_dir = self
            .data_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("videos"));

        Ok(ResolvedConfig {
            api_key,
            model: self
                .model
                .clone()
                .unwrap_or_else(|| DEFAULT_MODEL.to_string()),
            size: self
                .size
                .clone()
                .unwrap_or_else(|| DEFAULT_SIZE.to_string()),
            seconds: self.seconds.unwrap_or(DEFAULT_SECONDS),
            data_dir,
            poll_interval: Duration::from_millis(
                self.poll_interval_ms.unwrap_or(DEFAULT_POLL_INTERVAL_MS),
            ),
        })
    }
}

#[derive(Debug, Clone)]
struct ResolvedConfig {
    api_key: String,
    model: String,
    size: String,
    seconds: u32,
    data_dir: PathBuf,
    poll_interval: Duration,
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
}

/// Primary entry point for managing Sora videos and continuations.
#[derive(Clone)]
pub struct VideoManager {
    client: SoraClient,
    config: ResolvedConfig,
}

impl VideoManager {
    /// Build a new manager from high-level configuration.
    pub fn new(config: SoraConfig) -> Result<Self, SoraError> {
        let resolved = config.resolve()?;
        let client = SoraClient::new(resolved.api_key.clone())?;
        Ok(Self {
            client,
            config: resolved,
        })
    }

    /// Ensure the data directory exists on disk.
    async fn ensure_data_dir(&self) -> Result<(), SoraError> {
        fs::create_dir_all(&self.config.data_dir).await?;
        Ok(())
    }

    fn video_path(&self, local_id: &str) -> PathBuf {
        self.config.data_dir.join(format!("{local_id}.mp4"))
    }

    fn metadata_path(&self, local_id: &str) -> PathBuf {
        self.config.data_dir.join(format!("{local_id}.json"))
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
        self.client
            .download_video(&metadata.remote_id, variant, output_path)
            .await
    }

    /// Generate a brand-new clip using the Sora API and persist the results locally.
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
        let mut builder = ApiCreateRequest::from_create(&self.config, &request);
        let job = self.client.create_video(&mut builder).await?;
        let job = self.wait_for_completion(job.id.clone()).await?;

        let video_path = self.video_path(&request.local_id);
        self.client
            .download_video(&job.id, VideoVariant::Video, &video_path)
            .await?;

        let metadata = VideoMetadata {
            local_id: request.local_id,
            remote_id: job.id,
            prompt: builder.prompt,
            model: builder.model,
            seconds: builder.seconds,
            size: builder.size,
            created_at: job.created_at,
            file_path: video_path,
            parent: None,
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

        let mut builder = ApiCreateRequest::from_continue(&self.config, &request, &parent);
        builder.input_reference_path = Some(last_frame_path.clone());

        let job = self.client.create_video(&mut builder).await?;
        let job = self.wait_for_completion(job.id.clone()).await?;

        let video_path = self.video_path(&request.local_id);
        self.client
            .download_video(&job.id, VideoVariant::Video, &video_path)
            .await?;

        let metadata = VideoMetadata {
            local_id: request.local_id,
            remote_id: job.id,
            prompt: builder.prompt,
            model: builder.model,
            seconds: builder.seconds,
            size: builder.size,
            created_at: job.created_at,
            file_path: video_path,
            parent: Some(parent.local_id),
        };

        self.save_metadata(&metadata).await?;

        // Clean up temporary frame file.
        let _ = fs::remove_file(last_frame_path).await;

        Ok(metadata)
    }

    /// Enumerate all locally stored clips.
    pub async fn list_videos(&self) -> Result<Vec<VideoMetadata>, SoraError> {
        self.ensure_data_dir().await?;
        let mut entries = Vec::new();
        let mut dir = fs::read_dir(&self.config.data_dir).await?;
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
            .config
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

    async fn wait_for_completion(&self, remote_id: String) -> Result<VideoJob, SoraError> {
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
                    sleep(self.config.poll_interval).await;
                }
            }
        }
    }

    async fn extract_last_frame(
        &self,
        video_path: &Path,
        local_id: &str,
    ) -> Result<PathBuf, SoraError> {
        let frame_path = std::env::temp_dir().join(format!("{local_id}_last.png"));
        let status = Command::new("ffmpeg")
            .arg("-sseof")
            .arg("-1")
            .arg("-i")
            .arg(video_path)
            .arg("-frames:v")
            .arg("1")
            .arg("-update")
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

impl From<&ResolvedConfig> for ApiCreateRequest {
    fn from(config: &ResolvedConfig) -> Self {
        Self {
            prompt: String::new(),
            model: config.model.clone(),
            seconds: config.seconds,
            size: config.size.clone(),
            input_reference_path: None,
        }
    }
}

impl ApiCreateRequest {
    fn from_create(config: &ResolvedConfig, request: &CreateVideoRequest) -> Self {
        let mut base: ApiCreateRequest = config.into();
        base.prompt = request.prompt.clone();
        base.model = request.model.clone().unwrap_or(config.model.clone());
        base.seconds = request.seconds.unwrap_or(config.seconds);
        base.size = request.size.clone().unwrap_or(config.size.clone());
        base
    }

    fn from_continue(
        config: &ResolvedConfig,
        request: &ContinueVideoRequest,
        parent: &VideoMetadata,
    ) -> Self {
        let mut base: ApiCreateRequest = config.into();
        base.prompt = request.prompt.clone();
        base.model = request
            .model
            .clone()
            .unwrap_or_else(|| parent.model.clone());
        base.seconds = request.seconds.unwrap_or(parent.seconds);
        base.size = request.size.clone().unwrap_or_else(|| parent.size.clone());
        base
    }
}

#[derive(Clone)]
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
