//! DOOM (2016) audio: Wwise soundbanks (.bnk), stream packages (.pck), the engine's event table and
//! event lookup, read from the user's own install. Format and engine notes: `gamedata/re/AUDIO.md`.
//!
//! ```no_run
//! # fn main() -> anyhow::Result<()> {
//! let lib = idaudio::AudioLibrary::open(std::path::Path::new(r"C:/Program Files (x86)/Steam/steamapps/common\DOOM"))?;
//! let mut state = idaudio::PlayState::new(1);
//! state.set_switch("locality", "first_person");
//! for cmd in state.post_event(&lib, idaudio::sound_event_id("play_wpn_sp_shotgun_fire"))? {
//!     if let idaudio::Command::Play { what, .. } = cmd {
//!         for v in what.voices() {
//!             let pcm = lib.decode_media(v.media)?; // 48 kHz i16, v.volume_db / v.pitch_cents to apply
//!             # let _ = pcm;
//!         }
//!     }
//! }
//! # Ok(()) }
//! ```

pub mod adpcm;
pub mod anim;
pub mod bnk;
pub mod curve;
pub mod engine;
pub mod events;
pub mod hash;
pub mod hirc;
pub mod library;
pub mod mix;
pub mod names;
pub mod pck;
pub mod play;
mod read;
pub mod wav;
pub mod wem;

pub use hash::{event_id, fnv1_lower, sound_event_id, sound_event_name};
pub use library::{AudioLibrary, MediaLocation, MediaRef, Switches, TreeNode, soundbank_dir};
pub use play::{Command, PlayState, Playable, Voice};
pub use engine::{Engine, Environment, Listener};
pub use mix::{Mixer, ReverbParams};
pub use wem::{Decoded, WemInfo, decode};

#[cfg(test)]
mod tests {
    #[test]
    fn library_can_be_shared_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<crate::AudioLibrary>();
        assert_send_sync::<crate::PlayState>();
    }
}
