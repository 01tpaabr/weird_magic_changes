# Bevy 0.19.1 fact sheet — top-down glyph grid (verified against v0.19.1 sources, examples, and migration guides)

Sources: `bevyengine/bevy@v0.19.1` raw files (`crates/*/src/*.rs`, `examples/2d/tilemap_chunk.rs`), docs.rs module index pages (all 200), bevy.org migration guides 0.16→0.17, 0.17→0.18, 0.18→0.19, bevy.org setup page, `gfx-rs/wgpu@v29.0.3` (Bevy 0.19.1 pins `wgpu = "29.0.3"`). Anything not verified is marked **UNVERIFIED**.

---

## 1. TilemapChunk — YES, per-tile colour tint is supported

**Crate/module:** `bevy_sprite_render` (feature `bevy_sprite_render`, part of the `2d` collection). Path in `bevy`: `bevy::sprite_render::{TilemapChunk, TilemapChunkTileData, TileData, TileOrientation, TilemapChunkPlugin, TilemapChunkMaterial}`. **NOT in prelude** — `sprite_render::prelude` only exports `ColorMaterial, MeshMaterial2d, SpriteMaterial`.

**Plugin:** `TilemapChunkPlugin` + `TilemapChunkMaterialPlugin` are added by `SpriteRenderPlugin`, which is in `DefaultPlugins` under `#[cfg(feature = "bevy_sprite_render")]`. Nothing to add manually.

```rust
// crates/bevy_sprite_render/src/tilemap_chunk/mod.rs (verbatim, docs stripped)
#[derive(Component, Clone, Debug, Default, Reflect, FromTemplate)]
#[component(immutable, on_insert = on_insert_tilemap_chunk)]   // <-- IMMUTABLE: re-insert to change
pub struct TilemapChunk {
    pub chunk_size: UVec2,          // in tiles
    pub tile_display_size: UVec2,   // world-unit size of one tile (NOT the texel size in the tileset)
    pub tileset: Handle<Image>,     // must be a 2D ARRAY texture (depth_or_array_layers = N tiles)
    pub alpha_mode: AlphaMode2d,
}
impl TilemapChunk { pub fn calculate_tile_transform(&self, position: UVec2) -> Transform }

#[derive(Clone, Copy, Debug, Reflect)]
pub struct TileData {
    pub tileset_index: u16,           // array layer index
    pub color: Color,                 // "White leaves the sampled texture color unchanged."
    pub visible: bool,
    pub orientation: TileOrientation, // Default | Rotate90 | Rotate180 | Rotate270 | MirrorH | MirrorHRotate90 | MirrorHRotate180 | MirrorHRotate270
}
impl TileData { pub fn from_tileset_index(tileset_index: u16) -> Self }  // color WHITE, visible true, Default orientation
impl Default for TileData  // tileset_index 0, Color::WHITE, visible: true, TileOrientation::Default

#[derive(Component, Clone, Debug, Deref, DerefMut, Reflect, Default)]
pub struct TilemapChunkTileData(pub Vec<Option<TileData>>);   // None = empty tile; len MUST == chunk_size.x*chunk_size.y
impl TilemapChunkTileData {
    pub fn tile_data_from_tile_pos(&self, tilemap_size: UVec2, position: UVec2) -> Option<&TileData>
    // index = tilemap_size.x * position.y + position.x
}
```

**Required/inserted components:** `TilemapChunk` declares no `#[require]`. Its `on_insert` hook reads `TilemapChunkTileData` **from the same entity at insert time** (warns and bails if missing or wrong length → chunk never renders), then inserts `Mesh2d(Rectangle::from_size(chunk_size * tile_display_size))` and `MeshMaterial2d<TilemapChunkMaterial>`. `Mesh2d` requires `Transform`. So spawn `(TilemapChunk{..}, TilemapChunkTileData(vec), Transform::…)` in one bundle.

**Indexing / orientation (verified in shader + `calculate_tile_transform` + 0.18 migration guide "Tilemap Chunk Layout"):** row-major, `index = y * chunk_size.x + x`, **origin bottom-left, y goes UP** (0.18 changed this from top-left). Shader: `tile_coord.y = chunk_size.y - 1 - tile_coord.y` after sampling UV (Rectangle mesh UV (0,0) is at top-left, verified in `bevy_mesh/src/primitives/dim2.rs`).

**Position:** the chunk mesh is a `Rectangle` **centred on the entity's Transform** (no Anchor). Tile (0,0) centre is at `Transform.translation + (-W/2 + tw/2, -H/2 + th/2)` where `W = chunk_size.x*tile_display_size.x`:

```rust
// calculate_tile_transform (x part; y symmetric, z = 0)
position.x as f32 * tds.x + tds.x / 2. - tds.x * chunk_size.x as f32 / 2.
```

**Per-tile colour:** yes — packed as sRGB `u8x4` (`color.to_srgba().to_u8_array()`), shader does `final_color = textureSample(tileset, sampler, local_uv, tile.tileset_index) * tile.color;` then `if final_color.a < 0.001 { discard; }`. A white glyph atlas with alpha, tinted per tile, works. **No per-tile background colour** — use a second chunk (solid-white layer index, tinted) underneath, or put a solid-fill layer in the tileset and spawn two chunks (`z` offset).

**Empty tiles:** `None` → `tileset_index = u16::MAX` sentinel, shader `discard`s. So max 65535 usable layers by encoding, but wgpu `Limits::default().max_texture_array_layers = 256` (`wgpu-types/src/limits.rs` v29.0.3; `downlevel` may be lower). `max_texture_dimension_2d = 8192` bounds the tile-data texture → chunk_size ≤ 8192 per axis; 160×90 is fine as **one chunk**.

**Updating every frame:** just mutate `TilemapChunkTileData` (via `&mut` / `DerefMut`). System `update_tilemap_chunk_indices` (in `Update`, `Changed<TilemapChunkTileData>`) repacks the whole Vec into the `Rgba16Uint` tile-data `Image` via `images.get_mut(..)` → `AssetEvent::Modified` → re-uploaded by `RenderAssetPlugin`. It re-validates the length each time. Cost: O(tiles) repack + upload per changed frame.

```rust
// examples/2d/tilemap_chunk.rs @ v0.19.1 (verbatim excerpts)
use bevy::{
    image::{ImageArrayLayout, ImageLoaderSettings},
    prelude::*,
    sprite_render::{TileData, TilemapChunk, TilemapChunkTileData},
};
App::new().add_plugins(DefaultPlugins.set(ImagePlugin::default_nearest()))

    let chunk_size = UVec2::splat(64);
    let tile_display_size = UVec2::splat(8);
    let tile_data: Vec<Option<TileData>> = (0..chunk_size.element_product())
        .map(|_| rng.random_range(0..5))
        .map(|i| { if i == 0 { None } else { Some(TileData::from_tileset_index(i - 1)) } })
        .collect();

    commands.spawn((
        TilemapChunk {
            chunk_size,
            tile_display_size,
            tileset: assets
                .load_builder()
                .with_settings(|settings: &mut ImageLoaderSettings| {
                    settings.array_layout = Some(ImageArrayLayout::RowCount { rows: 4 });
                })
                .load("textures/array_texture.png"),
            ..default()
        },
        TilemapChunkTileData(tile_data),
        UpdateTimer(Timer::from_seconds(0.1, TimerMode::Repeating)),
    ));
    commands.spawn(Camera2d);

fn update_tilemap(time: Res<Time>, mut query: Query<(&mut TilemapChunkTileData, &mut UpdateTimer)>, mut rng: ResMut<SeededRng>) {
    for (mut tile_data, mut timer) in query.iter_mut() {
        timer.tick(time.delta());
        if timer.just_finished() {
            for _ in 0..50 {
                let index = rng.random_range(0..tile_data.len());
                tile_data[index] = Some(TileData::from_tileset_index(rng.random_range(0..5)));
            }
        }
    }
}
// player placed on tile (0,0):
    let mut transform = chunk.calculate_tile_transform(UVec2::new(0, 0));
    transform.translation.z = 1.;
```

**Tileset layout:** a 2D array texture. Options: (a) load a vertically stacked PNG with `ImageLoaderSettings::array_layout = Some(ImageArrayLayout::RowCount{rows}|RowHeight{pixels}|GridCount{columns,rows})`; (b) runtime: build `Image::new(Extent3d{width: gw, height: gh, depth_or_array_layers: N}, TextureDimension::D2, bytes, fmt, usages)` where `bytes` is the layers concatenated (== a vertically stacked image's bytes; `reinterpret_stacked_2d_as_array` only rewrites `size`); or (c) build stacked 2D then `img.reinterpret_stacked_2d_as_array(N)?` (returns `Result`, needs `layers >= 2`, height divisible). Tile texel size = layer width × height; display size is independent. Shader binding is `texture_2d_array<f32>` — a plain 2D (1-layer) image will fail pipeline creation (UNVERIFIED exact error, but the bind group declares `dimension = "2d_array"`). Sampler comes from the tileset `Image.sampler` — set `ImageSampler::nearest()` on a runtime image (or use `ImagePlugin::default_nearest()` for `ImageSampler::Default`).

```rust
// TilemapChunkMaterial (internal, for reference)
pub struct TilemapChunkMaterial {
    pub alpha_mode: AlphaMode2d,
    #[texture(0, dimension = "2d_array")] #[sampler(1)] pub tileset: Handle<Image>,
    #[texture(2, sample_type = "u_int")]  pub tile_data: Handle<Image>,   // Rgba16Uint, size = chunk_size
}
// AlphaMode2d (bevy::sprite_render::AlphaMode2d): Opaque (default) | Mask(f32) | Blend
```
Use `AlphaMode2d::Blend` for glyphs with soft alpha; `Opaque`/`Mask` if every cell has a solid background (note the shader always `discard`s alpha < 0.001 anyway).

Caveats: `TilemapChunkMeshCache` is read but never populated (a fresh `Rectangle` mesh per chunk). `#[component(immutable)]` — to resize a chunk, insert a new `TilemapChunk` (re-runs hook, creates new material/image; old assets are dropped when handles drop).

---

## 2. Images (`bevy::image`, re-exported in prelude: `Image, ImagePlugin, TextureAtlas, TextureAtlasLayout`)

```rust
// crates/bevy_image/src/image.rs
pub struct Image {
    pub data: Option<Vec<u8>>,                      // YES Option<Vec<u8>> in 0.19
    pub data_order: TextureDataOrder,
    pub texture_descriptor: TextureDescriptor<Option<&'static str>, &'static [TextureFormat]>,
    pub sampler: ImageSampler,
    pub texture_view_descriptor: Option<TextureViewDescriptor<Option<&'static str>>>,
    pub asset_usage: RenderAssetUsages,
    pub copy_on_resize: bool,
}
pub fn new(size: Extent3d, dimension: TextureDimension, data: Vec<u8>, format: TextureFormat, asset_usage: RenderAssetUsages) -> Self
pub fn new_fill(size: Extent3d, dimension: TextureDimension, pixel: &[u8], format: TextureFormat, asset_usage: RenderAssetUsages) -> Self
pub fn reinterpret_stacked_2d_as_array(&mut self, layers: u32) -> Result<(), TextureReinterpretationError>
pub fn width(&self) -> u32; pub fn height(&self) -> u32; pub fn size(&self) -> UVec2
pub fn set_color_at(&mut self, x: u32, y: u32, color: Color) -> Result<(), TextureAccessError>
pub fn pixel_bytes_mut(&mut self, coords: UVec3) -> Result<&mut [u8], TextureAccessError>

pub enum ImageSampler { #[default] Default, Descriptor(ImageSamplerDescriptor) }
impl ImageSampler { pub fn linear() -> Self; pub fn nearest() -> Self }   // nearest = mag/min/mipmap Nearest
pub trait ToExtents { fn to_extents(self) -> Extent3d }  // impl for UVec2 (layers=1) and UVec3
pub struct ImagePlugin { pub default_sampler: ImageSamplerDescriptor }
impl ImagePlugin { pub fn default_linear() -> Self; pub fn default_nearest() -> Self }  // Default = linear
```
- `TextureFormat`, `TextureDimension`, `Extent3d`, `TextureUsages` come from `wgpu_types`, re-exported at `bevy::render::render_resource::*` (docs.rs 200) and `bevy::image::*` uses them. `TextureFormat::Rgba8UnormSrgb` = bytes are sRGB, sampled → linear (right for hand-authored colours); `Rgba8Unorm` = bytes taken as linear (use for masks / when you write linear values). Both valid for `Image::new`; `Image::new` `debug_assert!`s `data.len() == pixel_size * texel count`.
- `RenderAssetUsages` (`bevy::asset::RenderAssetUsages`, bitflags u8): `MAIN_WORLD = 1`, `RENDER_WORLD = 2`, `all()`, default = `MAIN_WORLD | RENDER_WORLD`. If **only** `RENDER_WORLD`, the CPU copy is taken (`take_gpu_data`) after upload and `data` becomes `None` — you cannot mutate it later. Keep the default to update via `get_mut`.
- `Assets<Image>::add(image) -> Handle<Image>`. `Assets<A>::get_mut(id) -> Option<AssetMut<'_, A>>` (0.19; was `&mut A`). `AssetMut` is `Mut`-like: queues `AssetEvent::Modified` on drop **only if actually deref-mutated**; `extract_render_asset` re-extracts on `Modified` → full re-upload of the image. (`get_mut_untracked` exists to skip the event.)
- Tilemap uses exactly this pattern (`images.get_mut(&material.tile_data)`, `data.as_mut()`, `clear()` + `extend_from_slice`).

```rust
let mut img = Image::new(
    Extent3d { width: 8, height: 16, depth_or_array_layers: 256 },   // 256 glyph layers of 8x16
    TextureDimension::D2, bytes, TextureFormat::Rgba8UnormSrgb,
    RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD);
img.sampler = ImageSampler::nearest();
let tileset: Handle<Image> = images.add(img);
```

---

## 3. Sprite fallback (`bevy::sprite`, prelude)

```rust
// crates/bevy_sprite/src/sprite.rs
#[derive(Component, Debug, Default, Clone, Reflect, FromTemplate)]
#[require(Transform, Visibility, VisibilityClass, Anchor)]
pub struct Sprite {
    pub image: Handle<Image>,
    pub texture_atlas: Option<TextureAtlas>,
    pub color: Color,
    pub flip_x: bool, pub flip_y: bool,
    pub custom_size: Option<Vec2>,
    pub rect: Option<Rect>,
    pub image_mode: SpriteImageMode,
}
impl Sprite {
    pub fn sized(custom_size: Vec2) -> Self
    pub fn from_image(image: Handle<Image>) -> Self
    pub fn from_atlas_image(image: Handle<Image>, atlas: TextureAtlas) -> Self
    pub fn from_color(color: impl Into<Color>, size: Vec2) -> Self
}
// Anchor is a separate REQUIRED component (since 0.17), newtype not enum:
pub struct Anchor(pub Vec2);
Anchor::CENTER | BOTTOM_LEFT | BOTTOM_CENTER | BOTTOM_RIGHT | CENTER_LEFT | CENTER_RIGHT | TOP_LEFT | TOP_CENTER | TOP_RIGHT  // consts; Default = CENTER

// crates/bevy_image/src/texture_atlas.rs
pub struct TextureAtlasLayout { pub size: UVec2, pub textures: Vec<URect> }
pub fn from_grid(tile_size: UVec2, columns: u32, rows: u32, padding: Option<UVec2>, offset: Option<UVec2>) -> Self
pub struct TextureAtlas { pub layout: Handle<TextureAtlasLayout>, pub index: usize }
```
`Assets<TextureAtlasLayout>` is registered by `TextureAtlasPlugin` (added by `SpriteRenderPlugin`). Per-cell sprites = 14 400 entities for 160×90 — works but is the slow path; prefer TilemapChunk.

```rust
let layout = layouts.add(TextureAtlasLayout::from_grid(UVec2::new(8, 16), 16, 16, None, None));
commands.spawn((Sprite { color: Color::srgb(1.,0.5,0.), ..Sprite::from_atlas_image(img.clone(), TextureAtlas { layout, index: 65 }) },
                Anchor::TOP_LEFT, Transform::from_xyz(x, y, 0.)));
```

---

## 4. Camera 2D & coordinates (`bevy::camera`, prelude)

```rust
// crates/bevy_camera/src/components.rs
#[derive(Component, Default, Reflect, Clone)]
#[require(Camera, Projection::Orthographic(OrthographicProjection::default_2d()), Frustum = ...)]
pub struct Camera2d;

// crates/bevy_camera/src/projection.rs
pub enum Projection { Perspective(PerspectiveProjection), Orthographic(OrthographicProjection), Custom(CustomProjection) }
pub struct OrthographicProjection { pub near: f32, pub far: f32, pub viewport_origin: Vec2, pub scaling_mode: ScalingMode, pub scale: f32, pub area: Rect }
impl OrthographicProjection { pub fn default_2d() -> Self /* near: -1000, far: 1000, scale 1.0, WindowSize, origin (0.5,0.5) */ }
pub enum ScalingMode { #[default] WindowSize, Fixed{width,height}, AutoMin{min_width,min_height}, AutoMax{max_width,max_height}, FixedVertical{viewport_height}, FixedHorizontal{viewport_width} }
```
- `WindowSize` (default): "1 world unit = 1 **logical** pixel when window scale factor is 1"; a 64-unit sprite renders 64 px. `scale`: "As scale increases, the apparent size of objects decreases" → zoom = `proj.scale = 1.0 / zoom`. y is up, x right, camera looks at -z; put the camera `Transform` at `(cx, cy, 0)` (2D default z is fine, near = -1000).
- Zoom system: `Query<&mut Projection, With<Camera2d>>` then `if let Projection::Orthographic(o) = &mut *p { o.scale *= f; }`.
- `Camera` (bevy_camera/src/camera.rs) fields: `viewport: Option<Viewport>, order: isize, is_active: bool, computed, output_mode, msaa_writeback, clear_color: ClearColorConfig, invert_culling, sub_camera_view`. **No `hdr` (→ `Hdr` component, 0.17), no `target` (→ `RenderTarget` component, 0.18).**
- `Camera::viewport_to_world_2d(&self, camera_transform: &GlobalTransform, viewport_position: Vec2) -> Result<Vec2, ViewportConversionError>`; `world_to_viewport(...) -> Result<Vec2, _>`; `logical_viewport_size() -> Option<Vec2>`; `physical_viewport_size() -> Option<UVec2>`; `target_scaling_factor() -> Option<f32>`.
- `ClearColor(pub Color)` — `Resource` (`bevy::camera::ClearColor`, prelude); per-camera `Camera.clear_color: ClearColorConfig { Default | Custom(Color) | None }`.
- `Msaa` — **Component** on the camera: `bevy::render::view::Msaa { Off = 1, Sample2, #[default] Sample4, Sample8 }`, in `bevy::render::prelude`. `commands.spawn((Camera2d, Msaa::Off));`

---

## 5. Window (`bevy::window`, prelude) and winit

```rust
// crates/bevy_window/src/lib.rs
pub struct WindowPlugin {
    pub primary_window: Option<Window>,               // default Some(Window::default())
    pub primary_cursor_options: Option<CursorOptions>,// default Some(default)  (cursor split out of Window in 0.17)
    pub exit_condition: ExitCondition,                // OnPrimaryClosed | OnAllClosed (default) | DontExit
    pub close_when_requested: bool,                   // default true
}
// crates/bevy_window/src/window.rs
#[derive(Component)] #[require(CursorOptions)]
pub struct Window { pub present_mode: PresentMode, pub mode: WindowMode, pub position: WindowPosition, pub resolution: WindowResolution,
    pub title: String, pub name: Option<String>, pub resizable: bool /* default true */, pub decorations: bool, pub transparent: bool, pub focused: bool,
    pub visible: bool, pub desired_maximum_frame_latency: Option<NonZero<u32>>, pub ime_enabled: bool, ... /* also titlebar_* macOS fields */ }
impl Window { pub fn width(&self)->f32; height()->f32; size()->Vec2 /* logical */; physical_width()->u32; physical_height()->u32; physical_size()->UVec2; scale_factor()->f32; cursor_position()->Option<Vec2> /* logical */; physical_cursor_position()->Option<Vec2> }
pub struct WindowResolution { /* private */ physical_width: u32, physical_height: u32, scale_factor_override: Option<f32>, scale_factor: f32 }  // default 1280x720
impl WindowResolution { pub fn new(physical_width: u32, physical_height: u32) -> Self;  // u32 since 0.17
    pub fn with_scale_factor_override(self, f32) -> Self; width()/height()/size() logical; physical_width()/physical_height()/physical_size(); scale_factor() }
impl From<(u32,u32)> / From<[u32;2]> / From<UVec2> for WindowResolution
pub enum PresentMode { AutoVsync = 0, AutoNoVsync = 1, #[default] Fifo = 2, FifoRelaxed = 3, Immediate = 4, Mailbox = 5 }   // (4,5 UNVERIFIED by grep but standard)
pub struct PrimaryWindow;   // marker Component
```
Query: `Query<&Window, With<PrimaryWindow>>` or `Single<&Window, With<PrimaryWindow>>`. Fields like `window.resolution.physical_width()` also exist.

Messages (all `#[derive(Message)]`, registered by `WindowPlugin` via `add_message::<T>()`), read with `MessageReader<T>`:
`WindowResized { window: Entity, width: f32, height: f32 }` (logical), `WindowScaleFactorChanged { window, scale_factor: f64 }`, `WindowBackendScaleFactorChanged { window, scale_factor: f64 }`, `WindowCloseRequested { window }`, `WindowFocused { window, focused: bool }`, `WindowMoved { window, position: IVec2 }`, `WindowCreated`, `WindowClosed`, `WindowClosing`, `RequestRedraw`.

**Quitting:** `bevy::app::AppExit` is a `Message` enum `{ #[default] Success, Error(NonZero<u8>) }` with `AppExit::error()`, `from_code(u8)`. Write with **`MessageWriter<AppExit>`** (0.17 renamed `EventWriter/EventReader/Events` → `MessageWriter/MessageReader/Messages`; method is `.write(...)`, also `write_batch`, `write_default`):

```rust
fn quit(keys: Res<ButtonInput<KeyCode>>, mut exit: MessageWriter<AppExit>) {
    if keys.just_pressed(KeyCode::KeyQ) { exit.write(AppExit::Success); }
}
```
0.19: `close_when_requested`, `exit_on_primary_closed`, `exit_on_all_closed` run in `Last` (set `ExitSystems`).

```rust
// crates/bevy_winit/src/winit_config.rs  — bevy::winit::WinitSettings (Resource)
pub struct WinitSettings { pub focused_mode: UpdateMode, pub unfocused_mode: UpdateMode }
WinitSettings::game()        // Continuous / reactive_low_power(1/60 s)
WinitSettings::desktop_app() // reactive(5 s) / reactive_low_power(60 s)
WinitSettings::mobile()
pub enum UpdateMode { Continuous, Reactive { wait: Duration, react_to_device_events: bool, react_to_user_events: bool, react_to_window_events: bool } }
UpdateMode::reactive(wait: Duration); UpdateMode::reactive_low_power(wait: Duration)  // low_power: no device events
```
`Default for WinitSettings` = Continuous/Continuous (line 34-35). Insert as resource: `.insert_resource(WinitSettings::game())`.

---

## 6. Input (`bevy::input`, prelude)

`Res<ButtonInput<KeyCode>>` methods (crates/bevy_input/src/button_input.rs): `pressed(T)`, `just_pressed(T)`, `just_released(T)`, `any_pressed(impl IntoIterator<Item=T>)`, `all_pressed`, `any_just_pressed`, `any_just_released`, `all_just_pressed`, `all_just_released`, `get_pressed()`, `get_just_pressed()`, `get_just_released()`, `clear_just_pressed`, `reset`, `reset_all`, `clear`.

`KeyCode` variants — all verified present: `KeyW KeyA KeyS KeyD ArrowUp ArrowDown ArrowLeft ArrowRight Space Period BracketLeft BracketRight KeyP KeyQ Escape Equal Minus NumpadAdd NumpadSubtract ShiftLeft ShiftRight`.

```rust
// crates/bevy_input/src/keyboard.rs
#[derive(Message, ...)]
pub struct KeyboardInput { pub key_code: KeyCode, pub logical_key: Key, pub state: ButtonState, pub text: Option<SmolStr>, pub repeat: bool, pub window: Entity }
pub enum Key { Character(SmolStr), /* … */ Space, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Escape, /* … */ }
```
Layout-independent `+`/`-`/`[`/`]`: `for ev in MessageReader<KeyboardInput>` → `if ev.state.is_pressed() && let Key::Character(c) = &ev.logical_key { match c.as_str() { "+" | "=" => .., "-" => .., "[" => .., "]" => .. } }` (`ButtonState::is_pressed` UNVERIFIED name; `ev.state == ButtonState::Pressed` is safe).

---

## 7. Time (`bevy::time`, prelude: `Time, Real, Virtual, Fixed, Timer, TimerMode`)

```rust
pub struct Time<T: Default = ()> { … }   // Res<Time> == Time<()>, the "current" clock (Virtual in Update, Fixed in FixedUpdate)
pub fn delta(&self) -> Duration; delta_secs() -> f32; delta_secs_f64() -> f64;
pub fn elapsed(&self) -> Duration; elapsed_secs() -> f32; elapsed_secs_f64() -> f64; elapsed_secs_wrapped() -> f32
Res<Time<Real>> (wall clock, unpaused); Res<Time<Virtual>> (pausable/scalable); Res<Time<Fixed>>
Timer::from_seconds(f32, TimerMode::Repeating).tick(delta).just_finished()   // Timer::is_finished()/is_paused() since 0.17
```

---

## 8. UI text / Text2d (`bevy::ui`, `bevy::text`, both in prelude)

```rust
// crates/bevy_ui/src/widget/text.rs
#[derive(Component, Default, Deref, DerefMut, ...)]
#[require(Node, TextLayout, TextFont, TextColor, LineHeight, LetterSpacing, TextNodeFlags, ContentSize, FontHinting::Enabled)]
pub struct Text(pub String);   impl Text { pub fn new(text: impl Into<String>) -> Self }
// crates/bevy_text/src/text.rs
pub struct TextFont { pub font: FontSource, pub font_size: FontSize, pub weight: FontWeight, pub width: FontWidth, pub style: FontStyle, pub font_smoothing: FontSmoothing, pub font_features: FontFeatures, pub font_variations: FontVariations }
pub enum FontSource { #[default] Handle(Handle<Font>), Family(SmolStr), /* generic families… */ }   // From<Handle<Font>>
pub enum FontSize { Px(f32), Vw(f32), Vh(f32), VMin(f32), /*…*/ }  // Default Px(20.), From<f32> => Px
impl TextFont { pub fn from_font_size(impl Into<FontSize>) -> Self; with_font(Handle<Font>); with_font_size(impl Into<FontSize>) }
pub struct TextColor(pub Color);  // Default WHITE, From<impl Into<Color>>
pub struct TextLayout { pub justify: Justify, pub linebreak: LineBreak }  // TextLayout::new(j, l), ::justify(j), ::linebreak(l), ::no_wrap()  (0.19 dropped new_with_ prefix)
pub struct TextSpan(pub String);  // child spans; TextSpan::new
pub enum LineHeight { Px(f32), RelativeToFont(f32) }  // separate COMPONENT since 0.18 (default RelativeToFont(1.2))
pub enum FontSmoothing { None, #[default] AntiAliased }
// crates/bevy_sprite/src/text2d.rs  (bevy::sprite::Text2d)
#[require(TextLayout, TextFont, TextColor, LineHeight, LetterSpacing, TextBounds, Anchor, Visibility, VisibilityClass, Transform, FontHinting::Disabled)]
pub struct Text2d(pub String);
// crates/bevy_ui/src/ui_node.rs
pub struct Node { pub display, pub box_sizing, pub position_type: PositionType, pub overflow, …, pub left: Val, pub right: Val, pub top: Val, pub bottom: Val, pub width: Val, pub height: Val, pub margin: UiRect, pub padding: UiRect, … }
pub enum PositionType { Relative, Absolute }   // Val::Px(f32), Val::Percent, Val::Auto, Vw/Vh/VMin/VMax
```
```rust
commands.spawn((Text::new("tick 0"), TextFont { font_size: FontSize::Px(14.), font_smoothing: FontSmoothing::None, ..default() },
                TextColor(Color::WHITE), Node { position_type: PositionType::Absolute, left: Val::Px(4.), bottom: Val::Px(2.), ..default() }));
fn update(mut q: Single<&mut Text, With<StatusLine>>) { q.0 = format!("tick {tick}"); }   // Text derefs to String; assigning triggers relayout
```
Default font: feature `default_font` (in `default_platform`, so on by default) embeds `FiraMono-subset.ttf` behind `Handle::<Font>::default()`; `TextFont::default()` uses it. Without the feature "no text will be rendered". 0.19 text engine is **Parley** (was cosmic-text); UI text is hinted, `Text2d` unhinted by default (`FontHinting`).

---

## 9. Assets from bytes (no `assets/` dir)

- `Assets<Image>` is created by `ImagePlugin` (`app.init_asset::<Image>()`), which needs `AssetPlugin` (`AssetServer`) present — keep `AssetPlugin` from `DefaultPlugins`. In a `Startup` system: `fn setup(mut images: ResMut<Assets<Image>>, mut commands: Commands) { let h = images.add(img); … }`.
- `AssetPlugin { file_path: String /* "assets" */, processed_file_path, watch_for_changes_override: Option<bool>, use_asset_processor_override: Option<bool>, mode: AssetMode /* Unprocessed */, meta_check: AssetMetaCheck, unapproved_path_mode }`.
- File watcher only exists with feature `file_watcher` (**not** in defaults; it *is* in the `dev` collection). Without it there is nothing to disable. With it, a missing dir logs `warn!("Skip creating file watcher because path {path:?} does not exist.")` (io/source.rs:565); set `watch_for_changes_override: Some(false)` to silence. `FileAssetReader::new(path, create_root)` — reader does not fail on a missing dir; **UNVERIFIED** whether 0.19.1 logs any warning for a missing `assets/` when nothing is loaded (grep of `io/file/mod.rs` found only an `error!` in `FileAssetWriter::new` when `create_root` fails).

---

## 10. Dev speed

- `dynamic_linking` is a real `bevy` feature (`Cargo.toml:280`, `["dep:bevy_dylib", "bevy_internal/dynamic_linking"]`). Bevy's setup page: use `cargo run --features bevy/dynamic_linking` (or `cargo add bevy -F dynamic_linking`) **only for dev**: "If you remove the dynamic_linking feature, your game executable can run standalone" — the dylib is found via rpath set by cargo, so the binary won't run outside `cargo run` / without the `.dylib` next to it. Windows needs opt-level tweaks ("too many exported symbols"); page lists **no macOS-specific caveat**; recommends the default linker on macOS (mold/lld unnecessary).
- Recommended profile (verbatim from setup page):
```toml
[profile.dev]
opt-level = 1
[profile.dev.package."*"]
opt-level = 3
```
- `bevy/dev` **exists**: `dev = ["debug", "bevy_dev_tools", "file_watcher"]` — it is *not* a "fast compile" switch; `dynamic_linking` is separate.
- Default features (0.19): `default = ["2d", "3d", "ui", "audio"]`; `2d = ["default_app", "default_platform", "2d_bevy_render", "scene", "picking"]`. `ui` and `audio` are no longer implied by `2d` (0.19). Minimal for this project: `bevy = { version = "0.19", default-features = false, features = ["2d", "ui"] }` (`ui` only if you want the status line; `default_font` comes from `default_platform`). Sprite/tilemap features: `bevy_sprite`, `bevy_sprite_render` (both inside `2d_bevy_render` — UNVERIFIED exact membership, but the `2d` example builds tilemaps).

---

## 11. Plugins / DefaultPlugins tweaks

```rust
App::new().add_plugins(DefaultPlugins
    .set(WindowPlugin { primary_window: Some(Window { title: "wmc".into(), resolution: WindowResolution::new(1280, 720),
                                                    present_mode: PresentMode::AutoVsync, resizable: true, ..default() }),
                        exit_condition: ExitCondition::OnPrimaryClosed, ..default() })
    .set(ImagePlugin::default_nearest())
    .set(bevy::log::LogPlugin { filter: "warn,wgpu=error,wmc=info".into(), level: bevy::log::Level::INFO, ..default() })
    .disable::<bevy::audio::AudioPlugin>())          // PluginGroupBuilder::disable<T: Plugin>(self) -> Self (plugin_group.rs:501)
```
`LogPlugin { filter: String, level: Level, custom_layer: fn(&mut App)->Option<BoxedLayer>, fmt_layer: fn(&mut App)->Option<BoxedFmtLayer> }`; `DEFAULT_FILTER` const exists. `PluginGroupBuilder::set<T: Plugin>(self, plugin: T)`, `add`, `disable`.

DefaultPlugins order (0.19.1 `default_plugins.rs`): PanicHandler, Log, TaskPool, FrameCount, Time, Transform, Diagnostics, Input, InputFocus(+Dispatch), Window, Accessibility, TerminalCtrlCHandler, Asset, WorldSerialization, Scene, Winit, Render, Image, Mesh, Camera, Light, PipelinedRendering, CorePipeline, PostProcess, AntiAlias, Sprite, SpriteRender, …

---

## 12. Migration deltas that matter here (old → new)

| Version | Change |
|---|---|
| 0.17 | Buffered events → **Messages**: `Event`→`Message` derive, `EventWriter/Reader/Events` → `MessageWriter/MessageReader/Messages`, `send_event*`→`write_message*`, `app.add_event`→`app.add_message`. Observers keep `Event`/`EntityEvent`; `Trigger<OnAdd,T>` → `On<Add,T>`. |
| 0.17 | `Sprite.anchor` removed → **`Anchor` required component**; enum variants → consts (`Anchor::TopLeft` → `Anchor::TOP_LEFT`, `Anchor::Custom(v)` → `Anchor(v)`). |
| 0.17 | `bevy_render` split: `Camera, Camera2d, Projection, OrthographicProjection, ClearColor` → `bevy::camera`; `Visibility` → `bevy::camera::visibility`; `Mesh, Mesh2d` → `bevy::mesh`; `Image` → `bevy::image`; shaders → `bevy::shader`. `Text2d` → `bevy::sprite`. Sprite *rendering* → `bevy_sprite_render` (`ColorMaterial`, `MeshMaterial2d`, `TilemapChunk`, `AlphaMode2d`). |
| 0.17 | `Window.cursor_options` → separate `CursorOptions` component / `WindowPlugin.primary_cursor_options`. `WindowResolution::new(u32, u32)` (was f32). `Camera.hdr` → `Hdr` component (0.19: `bevy::camera::Hdr`). `JustifyText`→`Justify`. `TextFont::from_font(h)` → `TextFont::from(h)`. `Timer::finished()`→`is_finished()`. |
| 0.18 | **`TilemapChunk` origin top-left → bottom-left.** `ImageLoaderSettings::array_layout` added. `Image::reinterpret_stacked_2d_as_array` returns `Result`. `LineHeight` moved out of `TextFont` into its own component. `Camera.target` → `RenderTarget` component. Cargo feature collections (`2d`, `3d`, `ui`, `default_app`, `default_platform`). `bevy_input` sources behind features (`keyboard`, `mouse`, … — enabled automatically via `bevy_window`). Sprite/mesh `Aabb` auto-updates. |
| 0.19 | `TextFont.font: FontSource` (`.into()` from `Handle<Font>`), `font_size: FontSize::Px(f32)`. `TextLayout::new_with_justify` → `TextLayout::justify`. `Assets::get_mut` → `AssetMut` (Modified only on real mutation). `AssetServer::load_with_settings` → `load_builder().with_settings(..).load(path)`. `Resource` is now a subtrait of `Component` — no type may derive both. `ui`/`audio` no longer implied by `2d`. `bevy_window` moved from `default_app` to `common_api`. Window exit systems run in `Last`. `Camera` `TextureFormat::bevy_default()` deprecated (render-internal). Component hook `on_replace` → `on_discard`. `bevy_text` on Parley; `system_font_discovery` feature for system fonts. |

**Bottom line for the glyph grid:** one `TilemapChunk { chunk_size: UVec2::new(160, 90), tile_display_size: UVec2::new(8, 16), tileset: <256-layer Rgba8UnormSrgb array of white glyphs, nearest sampler>, alpha_mode: AlphaMode2d::Blend }` + `TilemapChunkTileData(vec![Some(TileData{tileset_index, color: fg, ..default()}); 14400])` for glyphs, a second chunk with a solid-white layer tinted by `bg` at z-1 for backgrounds; write both `Vec`s each frame from the sim; smooth scroll = move the `Camera2d` `Transform` (world unit = logical px, y-up), zoom = `OrthographicProjection.scale` inside `Projection::Orthographic`.
