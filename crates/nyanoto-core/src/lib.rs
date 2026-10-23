//! Движок воспроизведения.
//! Поток движка декодирует в кольцо, звуковой поток только забирает. Интерфейс — командами и событиями.

mod convert;
mod engine;
mod eq;
mod exclusive;
mod feel;
mod flac_writer;
mod health;
mod inspect;
mod native;
mod opus;
mod output;
mod peaks;
mod queue;
mod source;
mod stretch;
mod transcode;

pub use convert::Speed;
pub use engine::{Engine, Event, Loudness, NowPlaying, OutputTarget, PREVIOUS_RESTARTS_AFTER, Repeat, State};
pub use inspect::{Analyzer, Inspection, Sensitivity, Verdict, inspect};
pub use native::dsd::pcm_rate as dsd_pcm_rate;
pub use native::ffmpeg::{Status as FfmpegStatus, set_enabled as set_ffmpeg_enabled, status as ffmpeg_status};
pub use native::{Component, Probe, components, probe};
pub use eq::{EqBand, Equalizer, FilterKind, MAX_BANDS};
pub use feel::{FEEL_PER_SECOND, Feel, feel};
pub use health::{FileHealth, verify_file};
pub use output::{Capture, default_device_name};
pub use peaks::{PEAKS_PER_SECOND, Peaks, peaks};
pub use queue::QueueItem;
pub use transcode::{ConvertOptions, TargetFormat, convert_file};
pub use source::{Chunk, OpenOptions, ReplayGain, TrackDecoder, TrackError, TrackInfo, guard};
