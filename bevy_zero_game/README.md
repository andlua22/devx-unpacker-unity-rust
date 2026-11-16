# bevy_zero_game (Pure Bevy 0.20.1 Rebuild — Phase 6)

Clean-room Pure-Bevy (`0.20.1` + Rust `std`) rebuild of `lite4.0.apk` (`Unity 2021.3.23f1c1`),
combining extracted Unity assets (`glTF`/`.glb`, WAV audio, retained uGUI `.json` panels & sprites,
decoded `.oc` configuration tables) with an authoritative 100-player `std::net` UDP + 2D spatial
`InterestGrid` replication core. Strictly zero third-party gameplay, UI widget, or netcode crates.

## Boot Flow & Phase 6 1:1 Retained uGUI (`AppState`)
- `Boot -> PreloadConfigs -> LauncherSplash` — Retains extracted `LoadingPanel.json` via
  [`BoundUiPanel`](file:///home/andlua/Project/DevX/bevy_zero_game/src/ui/panel_loader.rs)
  (`m_bg`, `m_Animation`, `m_loding_slider -> Fill Area -> Fill`, `txt_version`, `txt_loding_name`,
  `txt_loding_name1`); streams 40k+ localization rows, 16k+ `cfg.ItemCfg.json` entries, and
  Phase 1–5 configs on Bevy `IoTaskPool` (`resolve_assets_root()` works from any CWD).
- `LoginLobby` — Retains APK `LoginPanel.json` (`Last Island of Survival` overseas logo, cleanly
  laid-out right utility `VerticalLayoutGroup`, live server selector strip, agreement checkbox
  `ckb_read` + checkmark `com_set_duihao`, `btn_start`), interactive native `UiTextInput` callsign
  field (`ui/widgets.rs`), scrollable `LoginServerPanel.json` server picker (`serverList.xml` +
  `Localhost 100P UDP Host`), and `LanguagePanel.json` modal (`uk-UA`, `en-US`, 11 languages).
- `MainLobby` — Retains `MainPanel.json`, `EstablishPanel.json`, `BagAndBuildPanel.json`, and
  `SettingPanel.json` atop the 3D turntable hero & outpost showcase stage (`lobby.rs`,
  `ui/main_lobby.rs`, `bgm_loading.wav`):
  - **Main Lobby (`MainPanel`)**: Live currency & avatar header (`RoleMgr` Gold/Diamond/Coupons),
    daily supply claim, `MainBgCfg` theme switcher, bottom dock, and right-drag 3D orbit.
  - **Character (`EstablishPanel`)**: Dedicated off-screen 3D studio `RenderTarget::Image` camera
    (`LobbyHeroPortraitRt`) piped directly into `m_hero_tex` (`RawImage`), live `UiTextInput`
    nickname input, gender toggles, `EstablishRole` presets, 6-part modular outfit/head/hair swaps,
    and `SkinColorCfg` swatches.
  - **Armory & Bag (`BagAndBuildPanel`)**: Scrollable (`UiScrollArea`) firearm & melee catalog joined
    with `cfg.GunCfg.json` + `cfg.CloseWeaponCfg.json` + `cfg.ItemCfg.json` and mounted live to
    `Bip001 R Hand`.
  - **Settings (`SettingPanel`, `SettingMgr`, Input Remapping & A11y)**: Continuous `UiSlider`
    bars for Music & SFX volume, `UiToggle` High-Contrast HUD & Control Hints switches, FPS/Quality
    tiers, language switcher, `KeybindProfile` remapping, and atomic JSON save.
- `LoadingScene -> InGame` — `BattleLoadingPanel.json` transition deploying into the streamed
  Phase 3–5 island arena (`ingame.rs`, `world_physics.rs`, `survival.rs`, `ui/ingame_hud.rs`):
  - **Terrain Streaming & Pure-Bevy Collider**: Streams `a-55..d-02` island terrain chunks and bakes
    GLB triangle soup (`37,184` triangles) into an 8m spatial [`TerrainColliderGrid`] shared identically
    by client raycasts, `--headless-server`, and 100-player UDP physics (`WASD`, `Shift` sprint, `Space`
    jump, right-drag orbit camera clamped above ground).
  - **Combat, Gathering & Hotbar (`[1..0]`, `[Q]`, `[T]`)**: Equipped rifle (`[1]`), hatchet (`[2]`), and
    knife (`[3]`) swapped onto `Bip001 R Hand`; hitscan shooting/melee against remote players/bots,
    Phase 4 monsters/Port Boss, trees, ore boulders, and supply crates with tracer beams, multi-particle
    mesh bursts, and weapon/material-matched spatial WAV SFX + ambient `bgm_wenmingchongqi.wav`.
  - **Grid/Socket Building, Auto-Turrets & Upgrades (`[4..0]`, `[T]`, `[R]`, `[U]`, `[E]`)**: Snapped
    `diji_lv1` (`48m` ray reach + horizon fallback), `qiangbi_lv1`, `tianhuaban_lv1`, `menkuang_lv1`,
    `danshanmen_lv1`, `dijitaijie_lv1`, `gongzuotai_lv1`, and automated `jiqiangpaotai_10001` Turret
    pieces replicated via MTU-bounded (`<1100B`) `WorldOp` / `WorldStateSync` UDP packets with tier
    upgrades (`Lv.1..Lv.4`) and hinged `[E]` door toggle.
  - **AI Enemies, Port War-Mech Boss & Field Loot (`TurretMgr` / `BattleDropMgr`)**: Replicated zombies
    (`monster_xlt_01`), brutes (`boss_gangkoushouling_01_b`), and Port Boss (`boss_gangkoushouling_01` w/
    baked glTF clips) patrolling/aggroing players and targeted automatically by placed turrets; kills &
    airdrops spawn floating crates/drop bags (`wupin_madai_low`, `normalbox`, `seniorbox`) lootable via `[F]`.
  - **Electricity, Generators & Tesla Turrets (`ElectricityMgr` / `[G]` / `[T]`)**: `[G]` cycles
    Fuel Generator (`fadianji`) & Battery Pillar (`xudianchizhu_01`); `[T]` cycles Auto Turret &
    Tesla Turret (`diancipao_01`). Active generators (`sheshi_fadianji_yxz.wav`) fuel local 28m
    grids (`base_power_pct`), empower Tesla arcs (`sheshi_diancipaota_yxz.wav`), and refuel/toggle via `[F]` or crafting.
  - **Ground & Air Vehicles (`VehicleCfgMgr` / `[F]` Board)**: Replicated Offroad Buggy
    (`project_poqiche_01`), Armored Tank (`project_tanke_01`), and Armed Helicopter (`npc_wuzhuangzhishengji_1`)
    with Pure-Bevy kinematics (`VehicleDriveInput`), fuel drain, `Space`/`Ctrl`/`C` flight altitude,
    `Shift` boost, mounted cannon/horn, roadkill collisions, engine/boarding SFX, and vehicle HUD telemetry.
  - **Persistent Base Saves & Accessible BattlePanel HUD (`[F5]`)**: Atomic `BaseSaveData` snapshots
    (`~/.local/share/bevy_zero_game/base_save.json`), APK `1BattlePanel_*` framing, live rotating
    radar minimap (`MinimapBlip` pool for vehicles, generators/batteries, turrets, threats, and drops),
    weapon/tool slot icons, `[Tab]` crafting modal, `[H]` bandage heal, `[B]` 100-player stress-bot
    toggle, and high-contrast A11y HUD styling.
- `DevSandbox` (`F12`) — Asset Explorer (`F1` Scene Explorer, `F2` Battle Stage, `F3` Model &
  Anim Studio, `F4` UI Panels viewer).

## Run & Verify
```bash
# Client (Graphical, works from workspace root or crate dir)
cargo run --manifest-path bevy_zero_game/Cargo.toml --release

# Headless Visual Smoke Capture (Splash, Login, MainLobby, InGame)
cargo run --manifest-path bevy_zero_game/Cargo.toml -- --capture-smoke /tmp/smoke_shots

# Dedicated Headless 100-Player Server (MinimalPlugins + 30Hz FixedUpdate)
cargo run --manifest-path bevy_zero_game/Cargo.toml --release -- --headless-server 0.0.0.0:9211

# Clippy & Tests
cargo clippy --manifest-path bevy_zero_game/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path bevy_zero_game/Cargo.toml
```
