#[macro_use]
extern crate log;

mod decrypt;
mod fetch;

mod range_set;

pub use decrypt::AudioDecrypt;
pub use fetch::{
    AudioCacheEvent, AudioFetchParams, AudioFile, AudioFileError, AudioKeyFailure, KeySource,
    StreamLoaderController, advance_cache_epoch, audio_key, cache_epoch, clear_cached_audio,
    forget_audio_key, install_audio_cache, install_cache_event_sender, remove_audio_file,
    remove_cached_audio,
};
