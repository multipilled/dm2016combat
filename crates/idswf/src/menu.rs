//! The in-game pause / settings menu (generated/swf/menu_shell/menu_shell_ingame.bswf), driven like the game's
//! idMenuScreen_Shell_Pause / idMenuScreen_Shell_Settings with their idMenuWidget_Shell_*Settings lists and
//! idMenuDataSource_*Settings data sources (decoded in gamedata/re/SWF.md, "Menus").
//!
//! The pages, item order, labels, widget types, value ranges and cvar conversions are the exe's. What the menu
//! changes goes through [`CvarStore`]; the game side applies it. Pointer positions are GUI pixels of the
//! `gui_w` x `gui_h` GUI passed to [`Menu::update`] (the window), the same space the renderer draws in.

use anyhow::Result;

use crate::assets::Assets;
use crate::player::{ObjId, Player, Value};
use crate::render::sprite_bounds;

/// Where the menu reads and writes cvars and key binds.
pub trait CvarStore {
    fn cvar(&self, name: &str) -> Option<String>;
    fn set_cvar(&mut self, name: &str, value: &str);
    /// Back to the install's value (REVERT TO DEFAULT).
    fn reset_cvar(&mut self, _name: &str) {}
    /// Every key that has a bind, in key-number order (keyboard keys, then mouse buttons and wheel): (engine key
    /// name, `_action`s of the bind).
    fn binds(&self) -> Vec<(String, Vec<String>)> {
        Vec::new()
    }
    /// `bind <key> "<actions>"`: replaces what the key does ("" unbinds it).
    fn set_bind(&mut self, _key: &str, _actions: &str) {}
    /// Back to default.cfg's binds (`resourceExec default.cfg` after clearing the screen's bindset).
    fn reset_binds(&mut self) {}
}

/// What the game should do (the pause menu's buttons).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuEvent {
    Resume,
    LoadCheckpoint,
    RestartMission,
    ExitToMainMenu,
    ExitToDesktop,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MenuInput {
    Up,
    Down,
    Left,
    Right,
    Accept,
    Back,
    /// Mouse wheel notches (positive = up).
    Wheel(i32),
    /// Pointer moved to GUI pixels.
    Pointer(f32, f32),
    Press,
    Release,
    /// An input pressed while the key bindings screen waits for one (engine key name, e.g. "W", "MOUSE2").
    Key(String),
    /// The page's second prompt: REVERT TO DEFAULT on a settings page, DEFAULTS on the key bindings screen.
    Defaults,
}

/// How a slider's 0..100 widget position maps to its cvar (the data sources' SetField/GetField).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliderMap {
    /// value = min + raw / 100 * (max - min), rounded to `step`.
    Linear { min: f32, max: f32, step: f32 },
    /// Volume: dB = 60 * (sqrt(raw / 100) - 1) (0x141000ed0); raw = 100 * (dB / 60 + 1)^2.
    VolumeDb,
}

/// Counter text of a slider.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SliderText {
    /// The value as an integer.
    Int,
    /// "%.1f" of the value.
    OneDecimal,
    /// "%d%%" of value * 100.
    Percent,
    /// The widget position 0..100.
    Raw,
}

/// A settings item's control (widget types 0 button, 1 slider, 3 toggle, 4 choice list).
#[derive(Debug, Clone, PartialEq)]
pub enum Control {
    /// Opens another screen (key bindings) or does nothing.
    Button,
    Slider { cvar: &'static str, map: SliderMap, text: SliderText, arrow: f32 },
    /// `invert`: the cvar is the opposite (r_skipFlares for "lens flare").
    Toggle { cvar: &'static str, invert: bool, on: &'static str },
    /// Choice strings (#str) and the cvar value of each.
    List { cvar: &'static str, choices: &'static [&'static str], values: &'static [&'static str] },
    /// Shown as the game shows it, but this testbed has nothing behind it (value text fixed).
    Unsupported { choices: &'static [&'static str] },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub label: &'static str,
    pub help: &'static str,
    pub control: Control,
}

const fn item(label: &'static str, help: &'static str, control: Control) -> Item {
    Item { label, help, control }
}

const OFF_ON: &[&str] = &["#str_menu_video_off", "#str_menu_video_on"];
const QUALITY: &[&str] = &["#str_menu_video_low", "#str_menu_video_medium", "#str_menu_video_high", "#str_menu_video_ultra", "#str_menu_video_nightmare"];

/// One settings category (left list) and its items (right list).
pub struct Page {
    pub label: &'static str,
    pub help: &'static str,
    /// Its list sprite under _center_settingsMenu.
    pub sprite: &'static str,
    pub items: Vec<Item>,
    pub enabled: bool,
}

/// The settings categories in the PC order of 0x1410736a0, with each page's SP item list for the current cvars.
pub fn pages(cvars: &dyn CvarStore) -> Vec<Page> {
    use Control::*;
    vec![
        Page {
            label: "#str_menu_settings_game_label",
            help: "#str_menu_settings_game_help",
            sprite: "gameSettings",
            enabled: true,
            items: {
                let mut v = Vec::new();
                // 0x14110c760 (SP): difficulty only below Nightmare (g_gameDifficulty < 3), with the tutorials toggle.
                let below_nightmare = cvars.cvar("g_gameDifficulty").and_then(|s| parse_num(&s)).unwrap_or(1.0) < 3.0;
                if below_nightmare {
                    v.push(item("#str_menu_game_difficulty_label", "#str_menu_game_difficulty_help", List {
                        cvar: "g_gameDifficulty",
                        choices: &["#str_menu_difficulty_easy_label", "#str_menu_difficulty_medium_label", "#str_menu_difficulty_hard_label"],
                        values: &["0", "1", "2"],
                    }));
                }
                v.extend([
                    item("#str_menu_game_autoswitchempty_label", "#str_menu_game_autoswitchempty_help", Toggle { cvar: "g_weaponAutoSwitchOnEmpty", invert: false, on: "1" }),
                    item("#str_menu_game_autoswitchnew_label", "#str_menu_game_autoswitchnew_help", Unsupported { choices: OFF_ON }),
                    item("#str_gameoptions_doom_classic_weapon_pose_header", "#str_gameoptions_doom_classic_weapon_pose_header_desc", Toggle { cvar: "g_weaponDoomClassicPose", invert: false, on: "1" }),
                    item("#str_gameoptions_crosshair_style_header", "#str_gameoptions_crosshair_style_header_desc", List {
                        cvar: "g_reticleMode",
                        choices: &["#str_gameoptions_crosshair_style_full", "#str_gameoptions_crosshair_style_dot", "#str_gameoptions_crosshair_style_none"],
                        values: &["0", "1", "2"],
                    }),
                    item("#str_gameoptions_glory_kill_highlight", "#str_gameoptions_glory_kill_highlight_desc", Toggle { cvar: "g_setting_gk_highlight", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_toggle", "#str_gameoptions_hud_toggle_desc", Toggle { cvar: "g_setting_hud_show", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_compass", "#str_gameoptions_hud_compass_desc", Toggle { cvar: "g_setting_compass", invert: false, on: "1" }),
                    item("#str_gameoptions_boss_health", "#str_gameoptions_boss_health_desc", Toggle { cvar: "g_setting_boss_health", invert: false, on: "1" }),
                    // Photo mode is listed only once a profile flag (0x170fa & 8) is set; left out.
                    item("#str_gameoptions_hud_interaction_prompt", "#str_gameoptions_hud_interaction_prompt_desc", Toggle { cvar: "g_setting_interact_prompt", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_objective_notification", "#str_gameoptions_hud_objective_notification_desc", Toggle { cvar: "g_setting_objectiveUpdate", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_objective_poi", "#str_gameoptions_hud_objective_poi_desc", Toggle { cvar: "g_setting_objectiveMarkers", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_notifications", "#str_gameoptions_hud_notifications_desc", Toggle { cvar: "g_setting_hudNotifications", invert: false, on: "1" }),
                    item("#str_gameoptions_hud_combat_scoring", "#str_gameoptions_hud_combat_scoring_desc", Toggle { cvar: "g_setting_combatScoring", invert: false, on: "1" }),
                ]);
                if below_nightmare {
                    v.push(item("#str_gameoptions_hud_tutorials", "#str_gameoptions_hud_tutorials_desc", Toggle { cvar: "g_setting_tutorials", invert: false, on: "1" }));
                }
                v
            },
        },
        Page { label: "#str_menu_settings_controller_label", help: "#str_menu_settings_controller_help", sprite: "controllerSettings", enabled: false, items: vec![] },
        Page {
            label: "#str_menu_settings_mouse_keyboard_label",
            help: "#str_menu_settings_mouse_keyboard_help",
            sprite: "mouseKeyboardSettings",
            enabled: true,
            items: vec![
                item("#str_menu_pc_controls_bindings_label", "#str_menu_pc_controls_bindings_help", Button),
                item("#str_menu_pc_controls_invert_mouse_label", "#str_menu_pc_controls_invert_mouse_help", Toggle { cvar: "in_invertLook", invert: false, on: "1" }),
                item("#str_menu_pc_controls_mouse_sensitivity_label", "#str_menu_pc_controls_mouse_sensitivity_help", Slider {
                    cvar: "m_sensitivity",
                    map: SliderMap::Linear { min: 0.1, max: 20.0, step: 0.1 },
                    text: SliderText::OneDecimal,
                    arrow: 0.1,
                }),
            ],
        },
        Page {
            label: "#str_menu_settings_audio_label",
            help: "#str_menu_settings_audio_help",
            sprite: "audioSettings",
            enabled: true,
            items: vec![
                item("#str_menu_audio_master_label", "#str_menu_audio_master_help", Slider { cvar: "s_volume_dB", map: SliderMap::VolumeDb, text: SliderText::Raw, arrow: 1.0 }),
                item("#str_menu_audio_music_label", "#str_menu_audio_music_help", Slider { cvar: "s_volume_music", map: SliderMap::VolumeDb, text: SliderText::Raw, arrow: 1.0 }),
                item("#str_menu_audio_sfx_label", "#str_menu_audio_sfx_help", Slider { cvar: "s_volume_sfx", map: SliderMap::VolumeDb, text: SliderText::Raw, arrow: 1.0 }),
                item("#str_menu_audio_vo_label", "#str_menu_audio_vo_help", Slider { cvar: "s_volume_vo", map: SliderMap::VolumeDb, text: SliderText::Raw, arrow: 1.0 }),
                item("#str_gameoptions_hud_subtitles", "#str_gameoptions_hud_subtitles_desc", Toggle { cvar: "g_setting_subtitles", invert: false, on: "1" }),
            ],
        },
        Page {
            label: "#str_menu_settings_video_label",
            help: "#str_menu_settings_video_help",
            sprite: "videoSettings",
            enabled: true,
            items: vec![
                item("#str_menu_system_field_aspect_ratio_label", "#str_menu_system_field_aspect_ratio_help", Unsupported { choices: &["16:9"] }),
                item("#str_menu_system_field_resolution_label", "#str_menu_system_field_resolution_help", Unsupported { choices: &["#str_menu_video_default"] }),
                item("#str_menu_system_field_fullscreen_label", "#str_menu_system_field_fullscreen_help", List {
                    cvar: "r_fullscreen",
                    choices: &["#str_menu_video_window", "#str_menu_video_fullscreen", "#str_menu_video_borderless_window"],
                    values: &["0", "1", "2"],
                }),
                item("#str_menu_video_verticalsync", "#str_menu_video_verticalsync_desc", List {
                    cvar: "r_swapInterval",
                    choices: &["#str_menu_video_adaptive", "#str_menu_video_off", "#str_menu_video_on"],
                    values: &["-1", "0", "1"],
                }),
                item("#str_menu_system_field_antialiasing_label", "#str_menu_system_field_antialiasing_help", Unsupported {
                    choices: &[
                        "#str_menu_video_off",
                        "#str_menu_video_antialiasing_low",
                        "#str_menu_video_antialiasing_smaalow",
                        "#str_menu_video_antialiasing_taa",
                        "#str_menu_video_antialiasing_taafxaa",
                        "#str_menu_video_antialiasing_taasmaa",
                        "#str_menu_video_antialiasing_medium",
                    ],
                }),
                item("#str_menu_video_monitor", "#str_menu_video_defaultmonitorindex_desc", Unsupported { choices: &["1"] }),
                item("#str_menu_system_field_fov_label", "#str_menu_system_field_fov_help", Slider {
                    cvar: "g_fov",
                    map: SliderMap::Linear { min: 90.0, max: 130.0, step: 1.0 },
                    text: SliderText::Int,
                    arrow: 0.4,
                }),
                item("#str_menu_system_field_gamma_label", "#str_menu_system_field_gamma_help", Slider {
                    cvar: "r_gamma",
                    map: SliderMap::Linear { min: 0.1, max: 2.8, step: 0.1 },
                    text: SliderText::OneDecimal,
                    arrow: 0.1,
                }),
                item("#str_menu_system_field_motion_blur_label", "#str_menu_system_field_motion_blur_help", Unsupported {
                    choices: &["#str_menu_video_off", "#str_menu_video_low", "#str_menu_video_medium", "#str_menu_video_high"],
                }),
                item("#str_menu_system_field_chromatic_aberration_label", "#str_menu_system_field_chromatic_aberration_help", Toggle {
                    cvar: "r_chromaticAberration",
                    invert: false,
                    on: "1",
                }),
                item("#str_menu_system_field_colorblind_mode_label", "#str_menu_system_field_colorblind_mode_help", List {
                    cvar: "r_colorBlindMode",
                    choices: &["#str_menu_video_off", "#str_menu_video_colorblind_deuteranopia", "#str_menu_video_colorblind_protanopia", "#str_menu_video_colorblind_tritanopia"],
                    values: &["0", "2", "1", "3"],
                }),
            ],
        },
        Page {
            label: "#str_menu_settings_advanced_label",
            help: "#str_menu_settings_advanced_help",
            sprite: "advancedVideoSettings",
            enabled: true,
            items: vec![
                item("#str_menu_video_overallquality", "#str_menu_video_overallquality_desc", Unsupported { choices: &["#str_menu_video_custom"] }),
                item("#str_menu_video_lightsquality", "#str_menu_video_lightsquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_shadingquality", "#str_menu_video_shadingquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_shadowsquality", "#str_menu_video_shadowsquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_particlesquality", "#str_menu_video_particlesquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_effectsquality", "#str_menu_video_effectsquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_directionalocclusionquality", "#str_menu_video_directionalocclusionquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_reflectionsquality", "#str_menu_video_reflectionsquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_decalquality", "#str_menu_video_decalquality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_decalfiltering", "#str_menu_video_decalfiltering_desc", Unsupported {
                    choices: &[
                        "#str_menu_video_decalfiltering_trilinear",
                        "#str_menu_video_decalfiltering_anisotropic2x",
                        "#str_menu_video_decalfiltering_anisotropic4x",
                        "#str_menu_video_decalfiltering_anisotropic8x",
                        "#str_menu_video_decalfiltering_anisotropic16x",
                    ],
                }),
                item("#str_menu_video_resolutionscale", "#str_menu_video_resolutionscale_desc", Unsupported { choices: &["100%"] }),
                item("#str_menu_video_dof", "#str_menu_video_dof_desc", Unsupported { choices: OFF_ON }),
                item("#str_menu_video_dof_aa", "#str_menu_video_dof_aa_desc", Unsupported { choices: OFF_ON }),
                item("#str_menu_video_motionblur_quality", "#str_menu_video_motionblur_quality_desc", Unsupported { choices: QUALITY }),
                item("#str_menu_video_sharpeningamount", "#str_menu_video_sharpeningamount_desc", Slider {
                    cvar: "r_sharpening",
                    map: SliderMap::Linear { min: 0.0, max: 4.0, step: 0.1 },
                    text: SliderText::OneDecimal,
                    arrow: 0.1,
                }),
                item("#str_menu_video_ui_opacity", "#str_menu_video_ui_opacity_desc", Slider {
                    cvar: "hud_globalAlpha",
                    map: SliderMap::Linear { min: 0.25, max: 1.0, step: 0.01 },
                    text: SliderText::Percent,
                    arrow: 0.01,
                }),
                item("#str_menu_video_hdrbloom", "#str_menu_video_hdrbloom_desc", Toggle { cvar: "r_hdrBloom", invert: false, on: "1" }),
                item("#str_menu_video_lensflare", "#str_menu_video_lensflare_desc", Toggle { cvar: "r_skipFlares", invert: true, on: "1" }),
                item("#str_menu_video_lensdirt", "#str_menu_video_lensdirt_desc", Toggle { cvar: "r_lensDirtRatio", invert: false, on: "1" }),
                item("#str_menu_video_virtualtexturingpagesize", "#str_menu_video_virtualtexturingpagesize_desc", Unsupported { choices: &["#str_menu_video_default"] }),
                item("#str_menu_video_filmgrain", "#str_menu_video_filmgrain_desc", Slider {
                    cvar: "r_filmGrainRatio",
                    map: SliderMap::Linear { min: 0.0, max: 4.0, step: 0.1 },
                    text: SliderText::OneDecimal,
                    arrow: 0.1,
                }),
                item("#str_menu_system_field_rendering_mode_label", "#str_menu_system_field_rendering_mode_help", List {
                    cvar: "r_renderMode",
                    choices: &["#str_menu_video_default", "#str_menu_video_rendering_realistic", "#str_menu_video_rendering_cinematic"],
                    values: &["0", "1", "2"],
                }),
                item("#str_menu_video_computeshaders", "#str_menu_video_computeshaders_desc", Unsupported { choices: OFF_ON }),
                item("#str_menu_video_playershadow", "#str_menu_video_playershadow_desc", Toggle { cvar: "r_skipPlayerShadow", invert: true, on: "1" }),
                item("#str_menu_video_performance_metrics", "#str_menu_video_performance_metrics_desc", Unsupported { choices: &["#str_menu_video_off"] }),
                item("#str_menu_video_graphicsapi", "#str_menu_video_graphicsapi_desc", Unsupported { choices: &["#str_menu_video_vulkan"] }),
                // Not in the game: this testbed's own switches.
                item("HDR", "HDR post process (dm2016combat)", Toggle { cvar: "r_hdrPostProcess", invert: false, on: "1" }),
                item("DEBUG OVERLAY", "Debug text overlay, also F10 (dm2016combat)", Toggle { cvar: "rancher_debugOverlay", invert: false, on: "1" }),
            ],
        },
    ]
}

/// The SP campaign pause list (idMenuScreen_Shell_Pause show 0x141069d20: resume, settings, load checkpoint,
/// restart mission (pause_addRestartMap 1), exit to main menu, exit to desktop).
const PAUSE: &[(&str, &str, Option<MenuEvent>)] = &[
    ("#str_resume_gamecap", "#str_resume_help", Some(MenuEvent::Resume)),
    ("#str_swf_settings", "#str_menu_root_settings_help", None),
    ("#str_swf_load_from_checkpoint_short", "#str_swf_load_from_checkpoint_help", Some(MenuEvent::LoadCheckpoint)),
    ("#str_menu_restart_map", "#str_menu_restart_map_help", Some(MenuEvent::RestartMission)),
    ("#str_swf_exit_to_menu", "#str_menu_root_exit_help", Some(MenuEvent::ExitToMainMenu)),
    ("#str_swf_exit_to_desktop", "#str_menu_root_exit_help", Some(MenuEvent::ExitToDesktop)),
];

/// The key bindings list of the single-player campaign (idMenuWidget_Shell_KeyBindings::Show 0x141112d00, bindset 0
/// list 0x14110e650): the screen's label base (the row shows `<base>_SP`, 0x1410c4280 with the "SP" suffix table at
/// 0x142fae220) and the `_action`s a key bound from the row gets.
pub const BIND_ROWS: &[(&str, &str)] = &[
    ("#str_moveforward_fps", "_moveforward"),
    ("#str_movebackwards_fps", "_moveback"),
    ("#str_moveleft_fps", "_moveLeft"),
    ("#str_moveright_fps", "_moveRight"),
    ("#str_jump_fps", "_jump"),
    ("#str_walk_fps", "_walk"),
    ("#str_crouch_fps", "_crouch"),
    ("#str_attack1_fps", "_attack1"),
    ("#str_zoom_fps", "_zoom _altfire"),
    ("#str_attack2_fps", "_attack2"),
    ("#str_changeWeapon_fps", "_changeWeapon"),
    ("#str_weapNext_fps", "_weapnext"),
    ("#str_weapPrev_fps", "_weapprev"),
    ("#str_reload_fps", "_reload"),
    ("#str_quickuse_fps", "_quickuse"),
    ("#str_quick0_fps", "_quick0"),
    ("#str_quick2_fps", "_quick2"),
    ("#str_use_fps", "_use"),
    ("#str_objectives_fps", "_objectives"),
    ("#str_quick3_fps", "_quick3"),
    ("#str_supermeter_fps", "_supermeter"),
    ("#str_weap0_fps", "_weap0"),
    ("#str_weap1_fps", "_weap1"),
    ("#str_weap2_fps", "_weap2"),
    ("#str_weap3_fps", "_weap3"),
    ("#str_weap4_fps", "_weap4"),
    ("#str_weap5_fps", "_weap5"),
    ("#str_weap6_fps", "_weap6"),
    ("#str_weap7_fps", "_weap7"),
];

/// input_maxBindings (cvar default 2, 0x14453d1a0): the most keyboard + mouse inputs one action keeps.
const MAX_BINDINGS: usize = 2;

const PAUSE_SCREEN: &str = "_root._center_pauseMenu";
const SETTINGS_SCREEN: &str = "_root._center_settingsMenu";
const BIND_SCREEN: &str = "_root._center_settingsMenu.keyBindings";
const BIND_INFO: &str = "_root._center_settingsMenu.keyBindings.info";
/// Rows of the key bindings list (binds.item0..item11).
const BIND_ROWS_SHOWN: usize = 12;
/// The command bar movie and its button slots (each: `txt_info` label, `img` icon).
const CMDBAR: &str = "_root.cmdBar._absLeft_cmdBar";
const CMD_ICONS: [&str; 4] = ["joy1", "joy2", "joy3", "joy4"];
/// Where the prompts sit in the bar (stage units).
const CMD_X0: f32 = -50.0;
const CMD_GAP: f32 = 80.0;
const DIALOG: &str = "_root.dialog";

/// The lower-cased, sorted `_action` names of an action string (a key's bind is compared by its action set).
fn action_set(s: &str) -> Vec<String> {
    let mut v: Vec<String> = s.split_whitespace().map(|a| a.to_ascii_lowercase()).collect();
    v.sort();
    v
}

/// Mouse buttons and the wheel are the key table's 0x11e..0x127 (SetBinding 0x141112ac0), the second list.
fn is_mouse_input(name: &str) -> bool {
    name.starts_with("MOUSE") || name.starts_with("MWHEEL")
}
/// Rows of the settings lists (item0..item11) and of the pause / category lists (item0..item9).
const LIST_ROWS: usize = 12;
const MENU_ROWS: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Screen {
    Pause,
    Settings,
    /// The key bindings list (opened from MOUSE AND KEYBOARD's KEY BINDINGS button).
    Bindings,
}

/// A modal confirmation (dialog.bswf; the game's GDM messages 0x13 / 0x28, text 0x14103... GetMessage 0x14102fa40).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Dialog {
    /// GDM_BINDING_ALREADY_SET: the key is bound to another action; ACCEPT rebinds it to the row's action.
    Rebind { key: String },
    /// GDM_BINDINGS_RESTORE: ACCEPT restores default.cfg's binds.
    Restore,
}

/// Which list has focus on the settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Categories,
    Items,
}

pub struct Menu {
    pub player: Player,
    pub open: bool,
    pub pages: Vec<Page>,
    screen: Screen,
    pause_focus: usize,
    page: usize,
    pane: Pane,
    item_focus: usize,
    scroll: usize,
    gui: (f32, f32),
    pointer: [f32; 2],
    /// Slider being dragged (page item index).
    drag: Option<usize>,
    /// Focus per list row as last shown, to play selecting / unselecting once.
    shown_focus: Vec<(String, bool)>,
    /// dialog.bswf, for the confirmations.
    pub dialog_player: Option<Player>,
    dialog: Option<Dialog>,
    dialog_focus: usize,
    shown_dialog_focus: Option<usize>,
    /// Key bindings screen: focused row (absolute), first shown row, the row waiting for a key.
    bind_focus: usize,
    bind_scroll: usize,
    waiting: Option<usize>,
    /// The slider widget position (page, item, 0..100) arrow presses last left it at.
    slider_pos: Option<(usize, usize, f32)>,
}

impl Menu {
    pub fn load(assets: &Assets) -> Result<Menu> {
        let player = assets.player("menu_shell/menu_shell_ingame")?;
        let mut m = Menu {
            player,
            open: false,
            pages: Vec::new(),
            screen: Screen::Pause,
            pause_focus: 0,
            page: 0,
            pane: Pane::Categories,
            item_focus: 0,
            scroll: 0,
            gui: (1920.0, 1080.0),
            pointer: [-1.0, -1.0],
            drag: None,
            shown_focus: Vec::new(),
            dialog_player: assets.player("dialog").map_err(|e| eprintln!("menu: dialog.bswf: {e:#}")).ok(),
            dialog: None,
            dialog_focus: 0,
            shown_dialog_focus: None,
            bind_focus: 0,
            bind_scroll: 0,
            waiting: None,
            slider_pos: None,
        };
        m.hide_all();
        m.hide_dialog();
        Ok(m)
    }

    /// Hides every root element; screens are shown one at a time.
    fn hide_all(&mut self) {
        let p = &mut self.player;
        let names: Vec<String> = p.sprite(p.root).map(|s| s.display.iter().filter(|d| d.inst.is_some()).map(|d| d.name.to_string()).collect()).unwrap_or_default();
        for n in names {
            p.set(&format!("_root.{n}"), "_visible", false);
        }
    }

    pub fn open(&mut self, cvars: &dyn CvarStore) {
        self.open = true;
        self.pages = pages(cvars);
        self.page = 0;
        self.pane = Pane::Categories;
        self.pause_focus = 0;
        self.dialog = None;
        self.waiting = None;
        self.hide_dialog();
        self.show(Screen::Pause, cvars);
    }

    pub fn close(&mut self) {
        self.open = false;
        self.dialog = None;
        self.waiting = None;
        self.hide_all();
        self.hide_dialog();
    }

    /// True while the key bindings screen waits for the next input: the host then sends [`MenuInput::Key`] for every
    /// key, mouse button and wheel notch (Esc included) instead of navigation.
    pub fn capturing(&self) -> bool {
        self.open && self.dialog.is_none() && self.screen == Screen::Bindings && self.waiting.is_some()
    }

    /// The movies to draw, bottom to top: the menu, then the confirmation dialog when one is up.
    pub fn players(&self) -> Vec<&Player> {
        let mut v = vec![&self.player];
        if self.dialog.is_some() {
            v.extend(self.dialog_player.as_ref());
        }
        v
    }

    fn hide_dialog(&mut self) {
        let Some(p) = self.dialog_player.as_mut() else { return };
        let names: Vec<String> = p.sprite(p.root).map(|s| s.display.iter().filter(|d| d.inst.is_some()).map(|d| d.name.to_string()).collect()).unwrap_or_default();
        for n in names {
            p.set(&format!("_root.{n}"), "_visible", false);
        }
        self.shown_dialog_focus = None;
    }

    fn show(&mut self, screen: Screen, cvars: &dyn CvarStore) {
        self.hide_all();
        self.screen = screen;
        self.drag = None;
        self.shown_focus.clear();
        let path = match screen {
            Screen::Pause => PAUSE_SCREEN,
            Screen::Settings | Screen::Bindings => SETTINGS_SCREEN,
        };
        let p = &mut self.player;
        p.set(path, "_visible", true);
        p.goto_and_play(path, "rollOnBack");
        match screen {
            Screen::Pause => {
                for n in ["bink", "arcade_content"] {
                    p.set(&format!("{PAUSE_SCREEN}.{n}"), "_visible", false);
                }
                p.goto_and_play(&format!("{PAUSE_SCREEN}.options"), "rollOn");
                p.set_text(&format!("{PAUSE_SCREEN}.options.header.txtVal.text"), "#str_menu_pause_title");
                for i in 0..MENU_ROWS {
                    let row = format!("{PAUSE_SCREEN}.options.item{i}");
                    match PAUSE.get(i) {
                        Some((label, ..)) => {
                            p.set(&row, "_visible", true);
                            p.set_text(&format!("{row}.info.label0.txtVal"), label);
                            p.set(&format!("{row}.info.snapcoin_price"), "_visible", false);
                        }
                        None => {
                            p.set(&row, "_visible", false);
                        }
                    }
                }
                p.set(&format!("{PAUSE_SCREEN}.options.scrollbar"), "_visible", false);
                self.set_prompts(&[]);
            }
            Screen::Settings | Screen::Bindings => {
                for n in ["keyBindings", "snapSettings", "dropDown", "audioSettings", "videoSettings", "advancedVideoSettings", "mouseKeyboardSettings", "controllerSettings", "gameSettings"] {
                    p.set(&format!("{SETTINGS_SCREEN}.{n}"), "_visible", false);
                }
                p.goto_and_play(&format!("{SETTINGS_SCREEN}.options"), "rollOn");
                p.set_text(&format!("{SETTINGS_SCREEN}.options.header.txtVal.text"), "#str_menu_settings_title");
                p.set_text(&format!("{SETTINGS_SCREEN}.title.subheader"), "");
                for i in 0..MENU_ROWS {
                    let row = format!("{SETTINGS_SCREEN}.options.item{i}");
                    match self.pages.get(i) {
                        Some(pg) => {
                            p.set(&row, "_visible", true);
                            p.set_text(&format!("{row}.info.label0.txtVal"), pg.label);
                            p.set(&format!("{row}.info.snapcoin_price"), "_visible", false);
                        }
                        None => {
                            p.set(&row, "_visible", false);
                        }
                    }
                }
                p.set(&format!("{SETTINGS_SCREEN}.options.scrollbar"), "_visible", false);
                if screen == Screen::Settings {
                    self.show_page(cvars);
                    self.set_prompts(&["#str_swf_select", "#str_swf_back", "#str_menu_system_field_revert_label"]);
                } else {
                    self.show_bindings(cvars);
                }
            }
        }
    }

    /// The bottom-left command bar (the game's cmdBar items: 0x141073f60 for the settings: SELECT 0xb, BACK 0xe and
    /// REVERT TO DEFAULT 0x22; 0x141112ee0 for the key bindings: SELECT, DONE, DEFAULTS). INTERIM: the game draws a
    /// key / mouse icon beside each label (shortcutKeys, 0x1410b5820); these are text only, in the bar's own slots.
    fn set_prompts(&mut self, prompts: &[&str]) {
        let p = &mut self.player;
        p.set("_root.cmdBar", "_visible", !prompts.is_empty());
        let names: Vec<String> = p.find(CMDBAR).and_then(|id| p.sprite(id)).map(|s| s.display.iter().filter(|d| d.inst.is_some()).map(|d| d.name.to_string()).collect()).unwrap_or_default();
        for n in names.iter().filter(|n| n.as_str() != "background") {
            p.set(&format!("{CMDBAR}.{n}"), "_visible", false);
        }
        let s = (self.gui.0 / p.swf.frame_width).min(self.gui.1 / p.swf.frame_height);
        let mut x = CMD_X0;
        for (i, label) in prompts.iter().enumerate() {
            let Some(icon) = CMD_ICONS.get(i) else { break };
            let path = format!("{CMDBAR}.{icon}");
            let p = &mut self.player;
            p.set(&path, "_visible", true);
            p.set(&path, "_x", x);
            p.set(&format!("{path}.img"), "_visible", false);
            p.set_text(&format!("{path}.txt_info"), label);
            // The next prompt starts after this label's measured width.
            if let Some(b) = self.bounds(&path) {
                x += (b[2] - b[0]) / s + CMD_GAP;
            }
        }
    }

    fn show_bindings(&mut self, cvars: &dyn CvarStore) {
        self.bind_focus = 0;
        self.bind_scroll = 0;
        self.waiting = None;
        let p = &mut self.player;
        p.set_text(&format!("{SETTINGS_SCREEN}.title.txtVal.text"), "#str_menu_bindings_title");
        p.set(BIND_SCREEN, "_visible", true);
        p.goto_and_play(BIND_SCREEN, "rollOn");
        p.set(&format!("{BIND_INFO}.waitingForKey"), "_visible", false);
        self.refresh_bindings(cvars);
        self.set_prompts(&["#str_swf_select", "#str_swf_done", "#str_swf_defaults"]);
    }

    /// Writes the visible rows' action labels and bound keys.
    fn refresh_bindings(&mut self, cvars: &dyn CvarStore) {
        let binds = cvars.binds();
        for row in 0..BIND_ROWS_SHOWN {
            let path = format!("{BIND_INFO}.binds.item{row}.info");
            let Some((label, actions)) = BIND_ROWS.get(self.bind_scroll + row) else {
                self.player.set(&format!("{BIND_INFO}.binds.item{row}"), "_visible", false);
                continue;
            };
            let text = self.binding_text(actions, &binds);
            let waiting = self.waiting == Some(self.bind_scroll + row);
            let p = &mut self.player;
            p.set(&format!("{BIND_INFO}.binds.item{row}"), "_visible", true);
            // The label is `<base>_SP` with its newlines removed (0x1410c4280: format "%s_%s", StripTrailing('\n')).
            let label = p.localize(&format!("{label}_SP")).replace('\n', "");
            p.set_text(&format!("{path}.action.txtVal"), &label);
            p.set_text(&format!("{path}.keyBinding.txtVal"), &text);
            p.set(&format!("{path}.bindChangeHighlight"), "_visible", waiting);
        }
        let (first, bar) = (self.bind_scroll, format!("{BIND_INFO}.scrollbar"));
        self.set_scrollbar(&bar, first, BIND_ROWS_SHOWN, BIND_ROWS.len());
    }

    /// The row's key text: keyboard keys then mouse inputs bound to exactly the row's actions, joined by " or "
    /// (0x1415af5f0 lists, #str_swf_or).
    fn binding_text(&self, actions: &str, binds: &[(String, Vec<String>)]) -> String {
        let want = action_set(actions);
        let names: Vec<&str> = binds.iter().filter(|(_, a)| action_set(&a.join(" ")) == want).map(|(k, _)| k.as_str()).collect();
        let (mouse, keys): (Vec<&str>, Vec<&str>) = names.into_iter().partition(|k| is_mouse_input(k));
        let sep = self.player.localize("#str_swf_or").into_owned();
        keys.into_iter().chain(mouse).map(|k| self.key_label(k)).collect::<Vec<_>>().join(&sep)
    }

    /// The key's display text (0x1415afad0): a keyboard key with a printable character shows it (the game asks the
    /// keyboard layout; INTERIM: US layout), the others the key table's `#str_key_<name>`; English upper-cases.
    pub fn key_label(&self, name: &str) -> String {
        let printable = match name {
            n if n.len() == 1 && n.as_bytes()[0].is_ascii_alphanumeric() => Some(n),
            "MINUS" => Some("-"),
            "EQUALS" => Some("="),
            "LBRACKET" => Some("["),
            "RBRACKET" => Some("]"),
            "BACKSLASH" => Some("\\"),
            "COMMA" => Some(","),
            "PERIOD" => Some("."),
            "SLASH" => Some("/"),
            "GRAVE" => Some("`"),
            _ => None,
        };
        let text = match printable {
            Some(c) => c.to_string(),
            None => self.player.localize(&format!("#str_key_{name}")).into_owned(),
        };
        text.to_ascii_uppercase()
    }

    /// Shows the current category's list on the right.
    fn show_page(&mut self, cvars: &dyn CvarStore) {
        let pg = &self.pages[self.page];
        let p = &mut self.player;
        for other in &self.pages {
            p.set(&format!("{SETTINGS_SCREEN}.{}", other.sprite), "_visible", false);
        }
        let list = format!("{SETTINGS_SCREEN}.{}", pg.sprite);
        p.set(&list, "_visible", true);
        p.goto_and_play(&list, "rollOn");
        // 0x141073e80 copies the selected category's label into the `title` widget.
        p.set_text(&format!("{SETTINGS_SCREEN}.title.txtVal.text"), pg.label);
        self.scroll = self.scroll.min(pg.items.len().saturating_sub(LIST_ROWS));
        self.refresh_items(cvars);
    }

    /// Writes labels and values into the visible rows of the current list (idMenuWidget_Shell option update 0x1410ac300).
    fn refresh_items(&mut self, cvars: &dyn CvarStore) {
        let pg = &self.pages[self.page];
        let list = format!("{SETTINGS_SCREEN}.{}.options", pg.sprite);
        let p = &mut self.player;
        for row in 0..LIST_ROWS {
            let path = format!("{list}.item{row}");
            let Some(it) = pg.items.get(self.scroll + row) else {
                p.set(&path, "_visible", false);
                continue;
            };
            p.set(&path, "_visible", true);
            let info = format!("{path}.info");
            p.set_text(&format!("{info}.label0.txtVal"), it.label);
            let ot = format!("{info}.optionType");
            let (kind, text, raw, on) = item_state(it, cvars);
            // optionType frame = widget type + 1 (sliderBar, sliderText, checkBox, dropDown, textInput, gradientSlider).
            match kind {
                0 => {
                    p.set(&ot, "_visible", false);
                }
                k => {
                    p.set(&ot, "_visible", true);
                    p.goto_and_stop(&ot, k as i32 + 1);
                }
            }
            match kind {
                1 => {
                    p.goto_and_stop(&format!("{ot}.sliderBar.bar"), raw.round().clamp(0.0, 100.0) as i32 + 1);
                    p.set_text(&format!("{ot}.sliderBar.Counter.txtVal"), &text);
                }
                3 => {
                    p.goto_and_stop(&format!("{ot}.checkBox"), if on { 2 } else { 1 });
                }
                4 => {
                    p.set_text(&format!("{ot}.dropDown.txtVal"), &text);
                }
                _ => {}
            }
        }
        let sb = format!("{list}.scrollbar");
        let (total, first) = (pg.items.len(), self.scroll);
        self.set_scrollbar(&sb, first, LIST_ROWS, total);
    }

    /// Feeds input; returns what the game should do.
    pub fn input(&mut self, ev: MenuInput, cvars: &mut dyn CvarStore) -> Option<MenuEvent> {
        if !self.open {
            return None;
        }
        if self.dialog.is_some() {
            self.dialog_input(ev, cvars);
            return None;
        }
        if self.screen == Screen::Bindings {
            self.bindings_input(ev, cvars);
            return None;
        }
        match ev {
            MenuInput::Key(_) => None,
            MenuInput::Defaults => {
                if self.screen == Screen::Settings {
                    self.revert_page(cvars);
                }
                None
            }
            MenuInput::Pointer(x, y) => {
                self.pointer = [x, y];
                if let Some(i) = self.drag {
                    self.drag_slider(i, cvars);
                } else {
                    self.hover(cvars);
                }
                None
            }
            MenuInput::Press => self.press(cvars),
            MenuInput::Release => {
                self.drag = None;
                None
            }
            MenuInput::Wheel(n) => {
                if self.screen == Screen::Settings {
                    let len = self.pages[self.page].items.len();
                    let max = len.saturating_sub(LIST_ROWS);
                    self.scroll = (self.scroll as i32 - n).clamp(0, max as i32) as usize;
                    self.refresh_items(cvars);
                }
                None
            }
            MenuInput::Up | MenuInput::Down => {
                let d: i32 = if ev == MenuInput::Up { -1 } else { 1 };
                match (self.screen, self.pane) {
                    (Screen::Pause, _) => self.pause_focus = (self.pause_focus as i32 + d).rem_euclid(PAUSE.len() as i32) as usize,
                    (Screen::Bindings, _) => {}
                    (Screen::Settings, Pane::Categories) => {
                        let n = self.pages.len() as i32;
                        let mut i = self.page as i32;
                        for _ in 0..n {
                            i = (i + d).rem_euclid(n);
                            if self.pages[i as usize].enabled {
                                break;
                            }
                        }
                        self.page = i as usize;
                        self.scroll = 0;
                        self.show_page(cvars);
                    }
                    (Screen::Settings, Pane::Items) => {
                        let len = self.pages[self.page].items.len();
                        if len > 0 {
                            self.item_focus = (self.item_focus as i32 + d).clamp(0, len as i32 - 1) as usize;
                            self.scroll_to_focus(cvars);
                        }
                    }
                }
                None
            }
            MenuInput::Left | MenuInput::Right => {
                let d = if ev == MenuInput::Left { -1 } else { 1 };
                if self.screen == Screen::Settings {
                    match self.pane {
                        Pane::Categories if d > 0 && !self.pages[self.page].items.is_empty() => {
                            self.pane = Pane::Items;
                            self.item_focus = self.scroll;
                        }
                        Pane::Items => self.adjust(self.item_focus, d, cvars),
                        _ => {}
                    }
                }
                None
            }
            MenuInput::Accept => match self.screen {
                Screen::Pause => self.activate_pause(self.pause_focus, cvars),
                Screen::Bindings => None,
                Screen::Settings => {
                    match self.pane {
                        Pane::Categories => {
                            if !self.pages[self.page].items.is_empty() {
                                self.pane = Pane::Items;
                                self.item_focus = self.scroll;
                            }
                        }
                        Pane::Items => self.adjust(self.item_focus, 1, cvars),
                    }
                    None
                }
            },
            MenuInput::Back => match self.screen {
                Screen::Pause => Some(MenuEvent::Resume),
                Screen::Bindings => None,
                Screen::Settings => {
                    if self.pane == Pane::Items {
                        self.pane = Pane::Categories;
                    } else {
                        self.show(Screen::Pause, cvars);
                        self.pause_focus = 1;
                    }
                    None
                }
            },
        }
    }

    fn activate_pause(&mut self, i: usize, cvars: &dyn CvarStore) -> Option<MenuEvent> {
        match PAUSE.get(i)? {
            (_, _, Some(e)) => Some(*e),
            (_, _, None) => {
                self.page = 0;
                self.pane = Pane::Categories;
                self.scroll = 0;
                self.item_focus = 0;
                self.show(Screen::Settings, cvars);
                None
            }
        }
    }

    fn scroll_to_focus(&mut self, cvars: &dyn CvarStore) {
        if self.item_focus < self.scroll {
            self.scroll = self.item_focus;
        } else if self.item_focus >= self.scroll + LIST_ROWS {
            self.scroll = self.item_focus + 1 - LIST_ROWS;
        }
        self.refresh_items(cvars);
    }

    /// Arrow / click adjustment of item `i` (AdjustField: sliders by their step, lists cycle, toggles flip).
    fn adjust(&mut self, i: usize, d: i32, cvars: &mut dyn CvarStore) {
        let Some(it) = self.pages[self.page].items.get(i).cloned() else { return };
        match &it.control {
            Control::Slider { cvar, map, arrow, .. } => {
                // The slider widget keeps its own position; the cvar only holds the quantised value (FOV in whole
                // degrees, 0.4 per press), so presses accumulate on the widget's position while the cvar is unchanged.
                let quantised = slider_raw(map, cvars.cvar(cvar).as_deref());
                let base = match self.slider_pos {
                    Some((pg, item, r)) if pg == self.page && item == i && slider_value(map, r) == slider_value(map, quantised) => r,
                    _ => quantised,
                };
                let raw = match map {
                    SliderMap::Linear { min, max, .. } => base + d as f32 * arrow / (max - min) * 100.0,
                    SliderMap::VolumeDb => base + d as f32 * arrow,
                }
                .clamp(0.0, 100.0);
                self.slider_pos = Some((self.page, i, raw));
                cvars.set_cvar(cvar, &slider_value(map, raw));
            }
            Control::Toggle { cvar, invert, on } => {
                let cur = toggle_on(cvars.cvar(cvar).as_deref(), *invert);
                let next = !cur;
                cvars.set_cvar(cvar, if next != *invert { on } else { "0" });
            }
            Control::List { cvar, values, .. } => {
                // INTERIM: the game opens the `dropDown` popup for a list (its placement and focus were not decoded); arrows
                // and clicks cycle the choices instead.
                let cur = values.iter().position(|v| Some(*v) == cvars.cvar(cvar).as_deref().map(|s| s.trim())).unwrap_or(0) as i32;
                let n = values.len() as i32;
                cvars.set_cvar(cvar, values[(cur + d).rem_euclid(n) as usize]);
            }
            Control::Button => {
                // The MOUSE AND KEYBOARD page's KEY BINDINGS button (idMenuWidget_Shell_KeyBindings, 0x141112d00).
                self.show(Screen::Bindings, cvars);
                return;
            }
            Control::Unsupported { .. } => {}
        }
        self.refresh_items(cvars);
    }

    /// GUI-space bounds of a sprite path.
    fn bounds(&self, path: &str) -> Option<[f32; 4]> {
        let id = self.player.find(path)?;
        sprite_bounds(&self.player, self.gui.0, self.gui.1, id)
    }

    fn inside(&self, path: &str) -> bool {
        let [x, y] = self.pointer;
        self.bounds(path).is_some_and(|b| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3])
    }

    /// Mouse focus follows the pointer (rows' hitbox sprites).
    fn hover(&mut self, cvars: &dyn CvarStore) {
        match self.screen {
            Screen::Bindings => {}
            Screen::Pause => {
                for i in 0..PAUSE.len() {
                    if self.inside(&format!("{PAUSE_SCREEN}.options.item{i}.info.hitbox")) {
                        self.pause_focus = i;
                    }
                }
            }
            Screen::Settings => {
                for i in 0..self.pages.len() {
                    if self.pages[i].enabled && self.inside(&format!("{SETTINGS_SCREEN}.options.item{i}.info.hitbox")) {
                        self.pane = Pane::Categories;
                        if self.page != i {
                            self.page = i;
                            self.scroll = 0;
                            self.show_page(cvars);
                        }
                    }
                }
                let list = format!("{SETTINGS_SCREEN}.{}.options", self.pages[self.page].sprite);
                for row in 0..LIST_ROWS.min(self.pages[self.page].items.len().saturating_sub(self.scroll)) {
                    if self.inside(&format!("{list}.item{row}.info.noFocusSelected")) {
                        self.pane = Pane::Items;
                        self.item_focus = self.scroll + row;
                    }
                }
            }
        }
    }

    fn press(&mut self, cvars: &mut dyn CvarStore) -> Option<MenuEvent> {
        if let Some(i) = self.prompt_at_pointer() {
            return self.input([MenuInput::Accept, MenuInput::Back, MenuInput::Defaults][i.min(2)].clone(), cvars);
        }
        self.hover(cvars);
        match self.screen {
            Screen::Pause => {
                let i = self.pause_focus;
                if self.inside(&format!("{PAUSE_SCREEN}.options.item{i}.info.hitbox")) {
                    return self.activate_pause(i, cvars);
                }
                None
            }
            Screen::Bindings => None,
            Screen::Settings => {
                if self.pane != Pane::Items {
                    return None;
                }
                let i = self.item_focus;
                let row = i - self.scroll;
                let ot = format!("{SETTINGS_SCREEN}.{}.options.item{row}.info.optionType", self.pages[self.page].sprite);
                match self.pages[self.page].items.get(i).map(|it| it.control.clone()) {
                    Some(Control::Slider { .. }) => {
                        if self.inside(&format!("{ot}.sliderBar.btnLess")) {
                            self.adjust(i, -1, cvars);
                        } else if self.inside(&format!("{ot}.sliderBar.btnMore")) {
                            self.adjust(i, 1, cvars);
                        } else if self.inside(&format!("{ot}.sliderBar")) {
                            self.drag = Some(i);
                            self.drag_slider(i, cvars);
                        }
                    }
                    Some(Control::Toggle { .. } | Control::List { .. } | Control::Button)
                        if self.inside(&format!("{SETTINGS_SCREEN}.{}.options.item{row}.info.noFocusSelected", self.pages[self.page].sprite)) =>
                    {
                        self.adjust(i, 1, cvars);
                    }
                    _ => {}
                }
                None
            }
        }
    }

    /// Slider position from the pointer between the bar's leftExtent and rightExtent (onDrag 0x2b).
    fn drag_slider(&mut self, i: usize, cvars: &mut dyn CvarStore) {
        if i < self.scroll || i >= self.scroll + LIST_ROWS {
            return;
        }
        let row = i - self.scroll;
        let bar = format!("{SETTINGS_SCREEN}.{}.options.item{row}.info.optionType.sliderBar", self.pages[self.page].sprite);
        // leftExtent / rightExtent draw nothing: their placements in the slider bar's coordinates are the track ends.
        let Some(bar_id) = self.player.find(&bar) else { return };
        let Some((origin, scale)) = idswf_origin(&self.player, self.gui, bar_id) else { return };
        let (lx, rx) = (self.player.get(&format!("{bar}.leftExtent"), "_x"), self.player.get(&format!("{bar}.rightExtent"), "_x"));
        let (Value::Num(lx), Value::Num(rx)) = (lx, rx) else { return };
        let (x0, x1) = (origin[0] + lx as f32 * scale, origin[0] + rx as f32 * scale);
        if x1 <= x0 {
            return;
        }
        let raw = ((self.pointer[0] - x0) / (x1 - x0) * 100.0).clamp(0.0, 100.0);
        if let Some(Control::Slider { cvar, map, .. }) = self.pages[self.page].items.get(i).map(|it| it.control.clone()) {
            cvars.set_cvar(cvar, &slider_value(&map, raw));
            self.refresh_items(cvars);
        }
    }

    /// Plays the rows' focus transitions and the help text, then advances the movie.
    pub fn update(&mut self, gui_w: f32, gui_h: f32, dt: f64) {
        self.gui = (gui_w, gui_h);
        if !self.open {
            return;
        }
        let mut rows: Vec<(String, bool)> = Vec::new();
        let help = match self.screen {
            Screen::Pause => {
                for i in 0..PAUSE.len() {
                    rows.push((format!("{PAUSE_SCREEN}.options.item{i}.info"), i == self.pause_focus));
                }
                PAUSE.get(self.pause_focus).map(|p| (format!("{PAUSE_SCREEN}.options.helpTooltip"), p.1))
            }
            Screen::Bindings => {
                for i in 0..self.pages.len() {
                    rows.push((format!("{SETTINGS_SCREEN}.options.item{i}.info"), i == self.page));
                }
                for row in 0..BIND_ROWS_SHOWN.min(BIND_ROWS.len().saturating_sub(self.bind_scroll)) {
                    rows.push((format!("{BIND_INFO}.binds.item{row}.info"), self.bind_scroll + row == self.bind_focus));
                }
                Some((format!("{SETTINGS_SCREEN}.options.helpTooltip"), "#str_swf_bindings_singleplayer_desc"))
            }
            Screen::Settings => {
                for i in 0..self.pages.len() {
                    rows.push((format!("{SETTINGS_SCREEN}.options.item{i}.info"), i == self.page));
                }
                let pg = &self.pages[self.page];
                let list = format!("{SETTINGS_SCREEN}.{}.options", pg.sprite);
                for row in 0..LIST_ROWS.min(pg.items.len().saturating_sub(self.scroll)) {
                    rows.push((format!("{list}.item{row}.info"), self.pane == Pane::Items && self.scroll + row == self.item_focus));
                }
                let h = match self.pane {
                    Pane::Categories => pg.help,
                    Pane::Items => pg.items.get(self.item_focus).map_or("", |it| it.help),
                };
                Some((format!("{SETTINGS_SCREEN}.options.helpTooltip"), h))
            }
        };
        let disabled: Vec<String> = if self.screen != Screen::Pause {
            self.pages.iter().enumerate().filter(|(_, pg)| !pg.enabled).map(|(i, _)| format!("{SETTINGS_SCREEN}.options.item{i}.info")).collect()
        } else {
            Vec::new()
        };
        for (path, focus) in &rows {
            let prev = self.shown_focus.iter().find(|(p, _)| p == path).map(|(_, f)| *f);
            if prev == Some(*focus) {
                continue;
            }
            // idMenuWidget_Button: gaining focus plays "selecting" (to the sel_ state), losing it "unselecting".
            let label = if disabled.contains(path) {
                "disabled"
            } else {
                match (prev, *focus) {
                    (None, true) => "sel_up",
                    (None, false) => "up",
                    (Some(_), true) => "selecting",
                    (Some(_), false) => "unselecting",
                }
            };
            self.player.goto_and_play(path, label);
        }
        self.shown_focus = rows;
        if let Some((tip, text)) = help {
            self.player.set_text(&format!("{tip}.txtOption.txtValue"), text);
            let shown = self.player.sprite(self.player.find(&tip).unwrap_or(self.player.root)).map(|s| s.frame).unwrap_or(0);
            if !(2..=15).contains(&shown) {
                self.player.goto_and_play(&tip, "show");
            }
        }
        self.player.update(dt);
        self.update_dialog(dt);
    }

    /// Plays the dialog's button focus transitions and advances its movie.
    fn update_dialog(&mut self, dt: f64) {
        let focus = self.dialog.is_some().then_some(self.dialog_focus);
        let shown = self.shown_dialog_focus;
        let Some(p) = self.dialog_player.as_mut() else { return };
        if focus != shown {
            for i in 0..2 {
                let path = format!("{DIALOG}.cmdBar.item{i}.info");
                let label = match (shown, focus) {
                    (None, Some(f)) => if f == i { "sel_up" } else { "up" },
                    (Some(_), Some(f)) => if f == i { "selecting" } else { "unselecting" },
                    _ => continue,
                };
                p.goto_and_play(&path, label);
            }
            self.shown_dialog_focus = focus;
        }
        if self.dialog.is_some() {
            Self::dialog_texts(p);
            p.update(dt);
            Self::dialog_texts(p);
        }
    }

    /// The buttons' labels (each focus state has its own text field, so they are set after every step).
    fn dialog_texts(p: &mut Player) {
        for (i, label) in ["#str_swf_accept", "#str_swf_cancel"].iter().enumerate() {
            p.set_text(&format!("{DIALOG}.cmdBar.item{i}.info.txtVal"), label);
        }
    }
}

impl Menu {
    /// REVERT TO DEFAULT (0x22, 0x141073f60): the page's cvars back to the install's values. INTERIM: the game's
    /// handler was not decoded; this resets every cvar the page's items edit.
    fn revert_page(&mut self, cvars: &mut dyn CvarStore) {
        for it in self.pages[self.page].items.clone() {
            match it.control {
                Control::Slider { cvar, .. } | Control::Toggle { cvar, .. } | Control::List { cvar, .. } => cvars.reset_cvar(cvar),
                _ => {}
            }
        }
        self.refresh_items(cvars);
    }

    /// Slider-style scrollbar of a list: the node's length is the shown fraction of the track, its offset the scroll
    /// position (the game's idMenuWidget_ScrollBar).
    fn set_scrollbar(&mut self, bar: &str, first: usize, shown: usize, total: usize) {
        self.player.set(bar, "_visible", total > shown);
        if total <= shown {
            return;
        }
        let s = (self.gui.0 / self.player.swf.frame_width).min(self.gui.1 / self.player.swf.frame_height);
        let Some(b) = self.bounds(&format!("{bar}.back")) else { return };
        let track = (b[3] - b[1]) / s;
        let len = (shown as f32 / total as f32).max(0.05);
        let off = first as f32 / (total - shown) as f32 * (1.0 - len) * track;
        self.player.set(&format!("{bar}.node"), "_yscale", len * 100.0);
        self.player.set(&format!("{bar}.node"), "_y", off);
    }

    fn prompt_at_pointer(&self) -> Option<usize> {
        let n = match self.screen {
            Screen::Pause => return None,
            Screen::Settings | Screen::Bindings => 3,
        };
        (0..n).find(|i| self.inside(&format!("{CMDBAR}.{}", CMD_ICONS[*i])))
    }

    fn bind_row_at_pointer(&self) -> Option<usize> {
        (0..BIND_ROWS_SHOWN.min(BIND_ROWS.len().saturating_sub(self.bind_scroll)))
            .find(|row| self.inside(&format!("{BIND_INFO}.binds.item{row}.info.noFocusSelected")))
            .map(|row| self.bind_scroll + row)
    }

    fn bindings_input(&mut self, ev: MenuInput, cvars: &mut dyn CvarStore) {
        if let Some(row) = self.waiting {
            // Every input is the new key (HandleWaitForBinding 0x14110e020); nothing else moves.
            if let MenuInput::Key(name) = ev {
                self.wait_key(row, &name, cvars);
            }
            return;
        }
        match ev {
            MenuInput::Pointer(x, y) => {
                self.pointer = [x, y];
                if let Some(row) = self.bind_row_at_pointer() {
                    self.bind_focus = row;
                }
            }
            MenuInput::Press => {
                if let Some(i) = self.prompt_at_pointer() {
                    self.bindings_input([MenuInput::Accept, MenuInput::Back, MenuInput::Defaults][i.min(2)].clone(), cvars);
                } else if let Some(row) = self.bind_row_at_pointer() {
                    self.bind_focus = row;
                    self.start_waiting(row, cvars);
                }
            }
            MenuInput::Wheel(n) => {
                let max = BIND_ROWS.len().saturating_sub(BIND_ROWS_SHOWN);
                self.bind_scroll = (self.bind_scroll as i32 - n).clamp(0, max as i32) as usize;
                self.refresh_bindings(cvars);
            }
            MenuInput::Up | MenuInput::Down => {
                let d: i32 = if ev == MenuInput::Up { -1 } else { 1 };
                self.bind_focus = (self.bind_focus as i32 + d).clamp(0, BIND_ROWS.len() as i32 - 1) as usize;
                if self.bind_focus < self.bind_scroll {
                    self.bind_scroll = self.bind_focus;
                } else if self.bind_focus >= self.bind_scroll + BIND_ROWS_SHOWN {
                    self.bind_scroll = self.bind_focus + 1 - BIND_ROWS_SHOWN;
                }
                self.refresh_bindings(cvars);
            }
            MenuInput::Accept => self.start_waiting(self.bind_focus, cvars),
            MenuInput::Back => {
                // Back to MOUSE AND KEYBOARD with the KEY BINDINGS button focused.
                self.show(Screen::Settings, cvars);
                self.pane = Pane::Items;
                self.item_focus = 0;
            }
            MenuInput::Defaults => self.open_dialog(Dialog::Restore),
            _ => {}
        }
    }

    /// A row was selected: the game shows "select a new key" (the waitingForKey widget) and highlights the row.
    fn start_waiting(&mut self, row: usize, cvars: &dyn CvarStore) {
        self.waiting = Some(row);
        // 0x14110e280 formats #str_swf_select_new_key with K_ESCAPE, which the GUI text prints as the Esc key's name.
        let esc = self.key_label("ESCAPE");
        let text = self.player.localize("#str_swf_select_new_key").replace("%s", &esc);
        let p = &mut self.player;
        p.set_text(&format!("{BIND_INFO}.waitingForKey.txtVal.text"), &text);
        p.set(&format!("{BIND_INFO}.waitingForKey"), "_visible", true);
        p.goto_and_play(&format!("{BIND_INFO}.waitingForKey"), 1);
        self.refresh_bindings(cvars);
    }

    fn stop_waiting(&mut self, cvars: &dyn CvarStore) {
        self.waiting = None;
        self.player.set(&format!("{BIND_INFO}.waitingForKey"), "_visible", false);
        self.refresh_bindings(cvars);
    }

    /// HandleWaitForBinding (0x14110e020): Esc cancels, Tab (and gamepad buttons) are ignored, any other input is
    /// bound to the row's actions; one that is already bound to them is unbound, one bound to something else asks first.
    fn wait_key(&mut self, row: usize, key: &str, cvars: &mut dyn CvarStore) {
        match key {
            "ESCAPE" => self.stop_waiting(cvars),
            "TAB" => {}
            _ => {
                let actions = BIND_ROWS[row].1;
                let existing = cvars.binds().into_iter().find(|(k, _)| k == key).map(|(_, a)| a.join(" ")).unwrap_or_default();
                if existing.is_empty() {
                    self.set_binding(key, actions, cvars);
                    self.stop_waiting(cvars);
                } else if action_set(actions).iter().all(|a| action_set(&existing).contains(a)) {
                    // FindText(existing, row actions): the key already does what the row binds.
                    self.set_binding(key, "", cvars);
                    self.stop_waiting(cvars);
                } else {
                    // GDM_BINDING_ALREADY_SET; the waiting prompt goes away while the dialog is up.
                    self.player.set(&format!("{BIND_INFO}.waitingForKey"), "_visible", false);
                    self.open_dialog(Dialog::Rebind { key: key.to_string() });
                }
            }
        }
    }

    /// SetBinding (0x141112ac0): "" unbinds the key; otherwise an action that already has input_maxBindings
    /// keyboard + mouse inputs first drops its oldest (the first keyboard key, or the first mouse input when the new
    /// key is a mouse one and there is one), then the key is bound (replacing what it did).
    fn set_binding(&mut self, key: &str, actions: &str, cvars: &mut dyn CvarStore) {
        if actions.is_empty() {
            cvars.set_bind(key, "");
            self.refresh_bindings(cvars);
            return;
        }
        let want = action_set(actions);
        let (mouse, keys): (Vec<String>, Vec<String>) =
            cvars.binds().into_iter().filter(|(_, a)| action_set(&a.join(" ")) == want).map(|(k, _)| k).partition(|k| is_mouse_input(k));
        if keys.len() + mouse.len() >= MAX_BINDINGS {
            let victim = if (!is_mouse_input(key) && !keys.is_empty()) || mouse.is_empty() { keys.first() } else { mouse.first() };
            if let Some(v) = victim {
                cvars.set_bind(v, "");
            }
        }
        cvars.set_bind(key, actions);
        self.refresh_bindings(cvars);
    }

    /// A confirmation (dialog.bswf): ACCEPT first, CANCEL second. INTERIM: the game's option order, default focus and
    /// title text (the SWF's own "NOTICE") were not decoded.
    fn open_dialog(&mut self, d: Dialog) {
        let msg = match &d {
            Dialog::Rebind { .. } => "#str_dlg_pc_bind_exists",
            Dialog::Restore => "#str_dlg_bind_restore",
        };
        self.dialog = Some(d);
        self.dialog_focus = 0;
        self.shown_dialog_focus = None;
        let Some(p) = self.dialog_player.as_mut() else { return };
        p.set(DIALOG, "_visible", true);
        p.goto_and_play(DIALOG, "rollOn");
        p.set_text(&format!("{DIALOG}.dialogMsg.info.txtInfo"), msg);
        for (i, label) in ["#str_swf_accept", "#str_swf_cancel"].iter().enumerate() {
            let item = format!("{DIALOG}.cmdBar.item{i}");
            p.set(&item, "_visible", true);
            p.set_text(&format!("{item}.info.txtVal"), label);
        }
        for i in 2..4 {
            p.set(&format!("{DIALOG}.cmdBar.item{i}"), "_visible", false);
        }
        p.set(&format!("{DIALOG}.cmdBar.timerInfo"), "_visible", false);
    }

    fn dialog_button_at_pointer(&self) -> Option<usize> {
        let p = self.dialog_player.as_ref()?;
        let [x, y] = self.pointer;
        (0..2).find(|i| {
            let id = p.find(&format!("{DIALOG}.cmdBar.item{i}.info"));
            id.and_then(|id| sprite_bounds(p, self.gui.0, self.gui.1, id)).is_some_and(|b| x >= b[0] && x <= b[2] && y >= b[1] && y <= b[3])
        })
    }

    fn dialog_input(&mut self, ev: MenuInput, cvars: &mut dyn CvarStore) {
        match ev {
            MenuInput::Up | MenuInput::Left => self.dialog_focus = 0,
            MenuInput::Down | MenuInput::Right => self.dialog_focus = 1,
            MenuInput::Accept => self.finish_dialog(self.dialog_focus == 0, cvars),
            MenuInput::Back => self.finish_dialog(false, cvars),
            MenuInput::Pointer(x, y) => {
                self.pointer = [x, y];
                if let Some(i) = self.dialog_button_at_pointer() {
                    self.dialog_focus = i;
                }
            }
            MenuInput::Press => {
                if let Some(i) = self.dialog_button_at_pointer() {
                    self.dialog_focus = i;
                    self.finish_dialog(i == 0, cvars);
                }
            }
            _ => {}
        }
    }

    fn finish_dialog(&mut self, accept: bool, cvars: &mut dyn CvarStore) {
        let d = self.dialog.take();
        self.hide_dialog();
        match (d, accept) {
            (Some(Dialog::Rebind { key }), true) => {
                // DIALOG_CMD_REBIND_KEY_ACCEPT: the key leaves its old action and goes to the row's.
                if let Some(row) = self.waiting {
                    self.set_binding(&key, BIND_ROWS[row].1, cvars);
                }
                self.stop_waiting(cvars);
            }
            (Some(Dialog::Rebind { .. }), false) => self.stop_waiting(cvars),
            (Some(Dialog::Restore), true) => {
                cvars.reset_binds();
                self.refresh_bindings(cvars);
            }
            _ => {}
        }
    }
}

/// (widget type, display text, slider position 0..100, toggle state) of an item.
fn item_state(it: &Item, cvars: &dyn CvarStore) -> (u32, String, f32, bool) {
    match &it.control {
        Control::Button => (0, String::new(), 0.0, false),
        Control::Slider { cvar, map, text, .. } => {
            let cur = cvars.cvar(cvar);
            let raw = slider_raw(map, cur.as_deref());
            let v = cur.as_deref().and_then(parse_num).unwrap_or(0.0);
            let t = match text {
                SliderText::Int => format!("{}", v.round() as i32),
                SliderText::OneDecimal => format!("{v:.1}"),
                SliderText::Percent => format!("{}%", (v * 100.0).round() as i32),
                SliderText::Raw => format!("{}", raw.round() as i32),
            };
            (1, t, raw, false)
        }
        Control::Toggle { cvar, invert, .. } => (3, String::new(), 0.0, toggle_on(cvars.cvar(cvar).as_deref(), *invert)),
        Control::List { cvar, choices, values } => {
            let cur = cvars.cvar(cvar);
            let i = values.iter().position(|v| Some(*v) == cur.as_deref().map(|s| s.trim())).unwrap_or(0);
            (4, choices[i].to_string(), 0.0, false)
        }
        Control::Unsupported { choices } => {
            if choices.len() == 2 && choices[0] == OFF_ON[0] {
                (3, String::new(), 0.0, false)
            } else {
                (4, choices.first().copied().unwrap_or("").to_string(), 0.0, false)
            }
        }
    }
}

/// A cvar value as a number (the cvar table writes floats like `1.0f`).
fn parse_num(s: &str) -> Option<f32> {
    s.trim().trim_end_matches('f').parse().ok()
}

fn toggle_on(v: Option<&str>, invert: bool) -> bool {
    let set = v.and_then(parse_num).is_some_and(|f| f != 0.0);
    set != invert
}

/// Widget position 0..100 of a slider's cvar value.
pub fn slider_raw(map: &SliderMap, v: Option<&str>) -> f32 {
    let v = v.and_then(parse_num).unwrap_or(0.0);
    match map {
        SliderMap::Linear { min, max, .. } => ((v - min) / (max - min) * 100.0).clamp(0.0, 100.0),
        SliderMap::VolumeDb => {
            let lin = (v / 60.0 + 1.0).clamp(0.0, 1.0);
            (lin * lin * 100.0).round()
        }
    }
}

/// The cvar value string for widget position `raw` (0..100).
pub fn slider_value(map: &SliderMap, raw: f32) -> String {
    match map {
        SliderMap::Linear { min, max, step } => {
            let v = min + raw / 100.0 * (max - min);
            let v = ((v / step).round() * step).clamp(*min, *max);
            let decimals = if *step >= 1.0 { 0 } else if *step >= 0.1 { 1 } else { 2 };
            format!("{v:.decimals$}")
        }
        SliderMap::VolumeDb => {
            let raw = raw.round().clamp(0.0, 100.0);
            let db = 60.0 * ((raw / 100.0).sqrt() - 1.0);
            format!("{db:.2}")
        }
    }
}

/// [`sprite_origin`] of `id` in a `gui` sized GUI.
fn idswf_origin(p: &Player, gui: (f32, f32), id: ObjId) -> Option<([f32; 2], f32)> {
    crate::render::sprite_origin(p, gui.0, gui.1, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bind_helpers() {
        // A key's bind is compared by its action set, whatever the order or case of the names.
        assert_eq!(action_set("_zoom _altfire"), action_set("_ALTFIRE _zoom"));
        assert_ne!(action_set("_zoom"), action_set("_zoom _altfire"));
        assert!(is_mouse_input("MOUSE2") && is_mouse_input("MWHEELUP") && !is_mouse_input("W"));
        assert_eq!(BIND_ROWS.len(), 29);
        assert_eq!(parse_num("1.0f"), Some(1.0));
        assert_eq!(parse_num(" -30 "), Some(-30.0));
    }

    #[test]
    fn slider_maps_round_trip() {
        let fov = SliderMap::Linear { min: 90.0, max: 130.0, step: 1.0 };
        assert_eq!(slider_value(&fov, 50.0), "110");
        assert!((slider_raw(&fov, Some("110")) - 50.0).abs() < 1e-4);
        assert_eq!(slider_value(&SliderMap::VolumeDb, 100.0), "0.00");
        assert_eq!(slider_value(&SliderMap::VolumeDb, 25.0), "-30.00");
        assert_eq!(slider_raw(&SliderMap::VolumeDb, Some("-30")), 25.0);
        let sens = SliderMap::Linear { min: 0.1, max: 20.0, step: 0.1 };
        assert_eq!(slider_value(&sens, 0.0), "0.1");
        assert_eq!(slider_value(&sens, 100.0), "20.0");
    }
}
