//! Checks against the user's own install; skipped when no install is found (set DOOM_DIR).

use idaudio::{AudioLibrary, Command, PlayState, Switches, event_id, sound_event_id};

fn library() -> Option<AudioLibrary> {
    let dir = idres::find_install()?;
    Some(AudioLibrary::open(&dir).expect("opening the install's sound data"))
}

#[test]
fn every_event_parses_and_resolves() {
    let Some(lib) = library() else { return };
    assert!(lib.parse_errors.is_empty(), "{:?}", &lib.parse_errors[..lib.parse_errors.len().min(5)]);
    assert_eq!(lib.event_ids().count(), lib.events_table.events.len());
    for e in &lib.events_table.events {
        assert!(lib.event(e.id).is_some(), "{} missing from the banks", e.name);
    }
}

#[test]
fn key_gameplay_sounds_decode() {
    let Some(lib) = library() else { return };
    let mut fp = Switches::default();
    fp.set("locality", "first_person");
    for (name, min_media) in [
        ("play_wpn_sp_shotgun_fire", 5),
        ("play_wpn_pistol_sp_fire", 4),
        ("play_fs_concrete", 10),
        ("footsteps/player/walk/sp_default", 10),
        ("play_jump_land_concrete", 5),
        ("player/special/player_suitjet_on", 4),
        ("play_wpn_sp_shotgun_dry_fire", 1),
    ] {
        let media = lib.resolve_event_with(sound_event_id(name), &fp).unwrap();
        assert!(media.len() >= min_media, "{name}: {} media", media.len());
        for m in media {
            let info = lib.media_info(m.media_id).unwrap();
            let d = lib.decode_media(m.media_id).unwrap();
            assert!(!d.truncated, "{name}: media {} truncated", m.media_id);
            assert_eq!(d.frames() as u64, info.total_frames());
            assert!(d.samples.iter().any(|&s| s != 0), "{name}: media {} silent", m.media_id);
        }
    }
}

#[test]
fn shotgun_fire_posts_dry_shot_and_reverb_tail() {
    let Some(lib) = library() else { return };
    let mut st = PlayState::new(7);
    st.set_switch("locality", "first_person");
    let cmds = st.post_event(&lib, event_id("Play_wpn_sp_shotgun_fire")).unwrap();
    let Command::Play { what, .. } = &cmds[0] else { panic!("expected play, got {cmds:?}") };
    let voices = what.voices();
    // One dry one-shot (Player bus) plus a front and a rear gun_env tail (Player_Weapon bus).
    assert_eq!(voices.len(), 3, "{what:#?}");
    assert!(voices.iter().any(|v| lib.bus_name(v.output_bus) == Some("Player") && (v.volume_db + 9.0).abs() < 1e-3));
    assert_eq!(voices.iter().filter(|v| lib.bus_name(v.output_bus) == Some("Player_Weapon")).count(), 2);
}

#[test]
fn volume_cvars_drive_bus_rtpcs() {
    let Some(lib) = library() else { return };
    let rtpc_on = |bus: &str, param: &str| {
        lib.bus_rtpcs(idaudio::fnv1_lower(bus)).iter().any(|r| r.rtpc == idaudio::fnv1_lower(param) && r.param == idaudio::hirc::prop::BUS_VOLUME)
    };
    assert!(rtpc_on("Master Audio Bus", "master_volume"));
    assert!(rtpc_on("Environmental", "sfx_volume"));
    assert!(rtpc_on("Music", "music_volume"));
    assert!(rtpc_on("VO", "vo_volume"));

    // Render the shotgun offline at sfx_volume 0, -20 and -60 dB (the s_volume_sfx cvar).
    let lib = std::sync::Arc::new(lib);
    let rms_at = |sfx: f32| {
        let mixer = std::sync::Arc::new(std::sync::Mutex::new(idaudio::Mixer::new(48000)));
        let mut eng = idaudio::Engine::new(lib.clone(), mixer.clone(), 3);
        eng.set_rtpc("sfx_volume", sfx);
        eng.post(event_id("Play_wpn_sp_shotgun_fire"), eng.player_emitter()).unwrap();
        let mut buf = vec![0.0f32; 2 * 480];
        let (mut sum, mut n) = (0.0f64, 0usize);
        for _ in 0..100 {
            eng.update();
            mixer.lock().unwrap().render(&mut buf);
            sum += buf.iter().map(|x| f64::from(x * x)).sum::<f64>();
            n += buf.len();
        }
        (sum / n as f64).sqrt()
    };
    let (full, minus20, silent) = (rms_at(0.0), rms_at(-20.0), rms_at(-60.0));
    assert!(full > 1e-4, "{full}");
    assert!(minus20 < full * 0.8 && minus20 > 0.0, "{minus20} vs {full}");
    assert!(silent < full * 1e-3, "{silent}");
}
