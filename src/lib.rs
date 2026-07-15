//! M3 — all 10 rails, a focus model, and arrow-key navigation.
//!
//! New since M2:
//! - render every rail (vertical stack of horizontal card rows)
//! - track the focused (rail, card) and draw a highlight on it
//! - `input(key)` moves focus, clamped at the edges
//! - instant scroll keeps the focused rail/card in view (smooth easing comes in M4)
//! - the fragment shader gains a solid-color mode, so we can draw the focus border
//! - textures are deduplicated: the 6 unique images upload once, keyed by URL

use glow::HasContext;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{HtmlCanvasElement, WebGl2RenderingContext};

// ---- Data model (parsed from content.json with serde) ----

#[derive(Deserialize)]
struct Content {
    rails: Vec<Rail>,
    #[serde(default)]
    featured: Vec<Featured>,
}

/// A hero/featured entry: a landscape backdrop plus a reference to an existing item
/// (so "More Info" reuses the rail/Meta machinery, and text reuses the item's strings).
#[derive(Deserialize)]
struct Featured {
    backdrop: String,
    rail: usize,
    card: usize,
}

/// A fetched page of rails (simulated API pagination).
#[derive(Deserialize)]
struct RailsPage {
    rails: Vec<Rail>,
}

#[derive(Deserialize)]
struct Rail {
    title: String,
    items: Vec<Item>,
}

#[derive(Deserialize)]
#[allow(dead_code)] // some fields aren't used until later milestones (Meta/Player)
struct Item {
    id: String,
    title: String,
    poster: String,
    video: String,
    description: String,
}

// Vertex shader: pixel positions -> clip space, passes texture coords through.
const VERTEX_SRC: &str = r#"#version 300 es
in vec2 a_pos;
in vec2 a_uv;
uniform vec2 u_resolution;
out vec2 v_uv;
void main() {
    vec2 clip = (a_pos / u_resolution) * 2.0 - 1.0;
    clip.y = -clip.y;
    gl_Position = vec4(clip, 0.0, 1.0);
    v_uv = a_uv;
}
"#;

// Fragment shader: two modes. u_use_tex=1 -> sample the poster texture;
// u_use_tex=0 -> fill with the flat u_color (used for the focus border).
const FRAGMENT_SRC: &str = r#"#version 300 es
precision mediump float;
in vec2 v_uv;
uniform sampler2D u_tex;
uniform int u_use_tex;
uniform vec4 u_color;
out vec4 outColor;
void main() {
    if (u_use_tex == 1) {
        outColor = texture(u_tex, v_uv);
    } else {
        outColor = u_color;
    }
}
"#;

#[wasm_bindgen]
pub struct Engine {
    gl: glow::Context,
    // Same GL context as `gl`, kept for web-sys-only calls (uploading an ImageBitmap
    // straight to a texture, skipping a JS pixel readback). State is shared with `gl`.
    webgl2: WebGl2RenderingContext,
    canvas: HtmlCanvasElement,
    program: glow::Program,
    vbo: glow::Buffer,
    res_loc: Option<glow::UniformLocation>,
    tex_loc: Option<glow::UniformLocation>,
    use_tex_loc: Option<glow::UniformLocation>,
    color_loc: Option<glow::UniformLocation>,
    res: (f32, f32),
    textures: HashMap<String, Tex>, // key (poster URL or "title:N") -> GPU texture
    content: Option<Content>,            // the ACTIVE tab's parsed content
    tabs_parsed: Vec<Option<Content>>,   // all tabs pre-parsed; switching is a pointer swap
    focus_rail: usize,
    // Each rail remembers its own focused card index (defaults to 0 = first item).
    // This is why moving down lands on a rail's first item, not a carried-over column.
    card_focus: Vec<usize>,
    // ---- animated values (eased toward targets each frame in tick) ----
    anim_y: f32,        // current vertical scroll (px)
    anim_x: Vec<f32>,   // current horizontal scroll per rail (px)
    focus_scale: f32,   // current scale of the focused card
    screen: Screen,     // Home / Meta / Player
    // Playback state, pushed from JS each frame while on Player.
    current: f32,
    duration: f32,
    paused: bool,
    // Home focus zone + hero carousel state.
    zone: Zone,
    hero_index: usize,
    hero_timer: f32, // ms accumulated toward the next auto-advance
    // Slide transition between hero items.
    hero_prev: usize, // the outgoing item
    hero_anim: f32,   // slide progress 0..1 (1 = settled on hero_index)
    hero_dir: f32,    // +1 = new slides in from the right, -1 = from the left
    // Top tab bar.
    tab_count: usize,
    tab_active: usize,
    tab_slide: f32, // horizontal offset of the tab content during a switch (eases to 0)
    scroll_busy: bool, // true while a scroll/slide is still animating toward its target
    frame_seq: u64, // monotonic frame counter, used to stamp texture recency (LRU)
}

// Animation tuning.
const SMOOTH_TAU_MS: f32 = 90.0; // smaller = snappier; ~ease-out over a few hundred ms
const FOCUS_SCALE_MAX: f32 = 1.10; // focused card grows 10%

// Hero banner.
const HERO_FRAC: f32 = 0.7; // hero occupies the top 70% of the viewport
const HERO_AUTO_MS: f32 = 5000.0; // auto-advance the featured carousel every 5s
const HERO_SLIDE_TAU_MS: f32 = 180.0; // slide transition speed (a touch slower than scroll)

// Top navigation tab bar height (px). Fixed strip at the very top.
const TAB_BAR_H: f32 = 64.0;

/// Which zone of the Home screen has focus.
#[derive(Clone, Copy, PartialEq)]
enum Zone {
    Tabs,
    Hero,
    Rails,
}

// Lazy-loading window: keep posters for rails within LOAD_RADIUS of focus loaded;
// free poster textures for rails beyond FREE_RADIUS (hysteresis avoids thrashing).
const LOAD_RADIUS: usize = 2;
// Horizontal window: cards loaded on each side of a rail's focused card (of its 20).
const HSPAN: usize = 6;
// While the scroll is farther than this from its target, we're "flying" — defer new
// image decodes until it settles (avoids decoding rails scrolled past).
const SETTLE_PX: f32 = 40.0;
// Max poster/backdrop textures kept resident (LRU). Keeps scroll-back instant while bounding
// GPU memory (~180 x 540 KB ≈ 95 MB). Lower it if a low-end TV shows memory pressure.
const MAX_POSTER_TEXTURES: usize = 180;

// Rail title (text) sizing, in pixels. Larger for TV legibility; the gap is wide
// enough that the focused card's scale-up + border never reaches into the title.
const TITLE_H: f32 = 38.0;
const TITLE_GAP: f32 = 30.0;

/// A GPU texture plus its source pixel size (needed to preserve text aspect ratio).
#[derive(Clone, Copy)]
struct Tex {
    texture: glow::Texture,
    w: f32,
    h: f32,
    used: u64, // frame_seq when last on/near screen (for LRU eviction)
}

/// Which screen the app is on (the state machine).
#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Home,
    Meta,
    Player,
}

#[wasm_bindgen]
impl Engine {
    pub fn new() -> Result<Engine, JsValue> {
        console_error_panic_hook::set_once();

        let document = web_sys::window()
            .ok_or("no window")?
            .document()
            .ok_or("no document")?;
        let canvas = document
            .get_element_by_id("app")
            .ok_or("no #app canvas")?
            .dyn_into::<HtmlCanvasElement>()?;
        let webgl2 = canvas
            .get_context("webgl2")?
            .ok_or("webgl2 not supported")?
            .dyn_into::<WebGl2RenderingContext>()?;
        let gl = glow::Context::from_webgl2_context(webgl2.clone());

        unsafe {
            let program = link_program(&gl, VERTEX_SRC, FRAGMENT_SRC)?;
            gl.use_program(Some(program));
            gl.enable(glow::BLEND);
            gl.blend_func(glow::SRC_ALPHA, glow::ONE_MINUS_SRC_ALPHA);

            let vao = gl.create_vertex_array().map_err(JsValue::from)?;
            gl.bind_vertex_array(Some(vao));
            let vbo = gl.create_buffer().map_err(JsValue::from)?;
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));

            let stride = 4 * 4;
            let a_pos = gl.get_attrib_location(program, "a_pos").ok_or("a_pos")?;
            gl.enable_vertex_attrib_array(a_pos);
            gl.vertex_attrib_pointer_f32(a_pos, 2, glow::FLOAT, false, stride, 0);
            let a_uv = gl.get_attrib_location(program, "a_uv").ok_or("a_uv")?;
            gl.enable_vertex_attrib_array(a_uv);
            gl.vertex_attrib_pointer_f32(a_uv, 2, glow::FLOAT, false, stride, 8);

            Ok(Engine {
                res_loc: gl.get_uniform_location(program, "u_resolution"),
                tex_loc: gl.get_uniform_location(program, "u_tex"),
                use_tex_loc: gl.get_uniform_location(program, "u_use_tex"),
                color_loc: gl.get_uniform_location(program, "u_color"),
                gl,
                webgl2,
                canvas,
                program,
                vbo,
                res: (1.0, 1.0),
                textures: HashMap::new(),
                content: None,
                tabs_parsed: Vec::new(),
                focus_rail: 0,
                card_focus: Vec::new(),
                anim_y: 0.0,
                anim_x: Vec::new(),
                focus_scale: 1.0,
                screen: Screen::Home,
                current: 0.0,
                duration: 0.0,
                paused: true,
                zone: Zone::Tabs,
                hero_index: 0,
                hero_timer: 0.0,
                hero_prev: 0,
                hero_anim: 1.0,
                hero_dir: 1.0,
                tab_count: 0,
                tab_active: 0,
                tab_slide: 0.0,
                scroll_busy: false,
                frame_seq: 0,
            })
        }
    }

    pub fn set_size(&mut self, width: u32, height: u32) {
        self.canvas.set_width(width);
        self.canvas.set_height(height);
        self.res = (width as f32, height as f32);
        unsafe {
            self.gl.viewport(0, 0, width as i32, height as i32);
        }
    }

    /// Parse a tab's JSON into the per-tab cache. If it's the active tab, make it live.
    /// Called by JS only when the slide has settled, so the parse never blocks the slide.
    pub fn load_tab(&mut self, i: usize, json: &str) -> Result<(), JsValue> {
        let content: Content =
            serde_json::from_str(json).map_err(|e| JsValue::from(e.to_string()))?;
        let rails = content.rails.len();
        if i == self.tab_active {
            self.content = Some(content);
            self.reset_layout(rails);
        } else if i < self.tabs_parsed.len() {
            self.tabs_parsed[i] = Some(content);
        }
        Ok(())
    }

    /// Is the active tab's content loaded (vs. showing a skeleton)?
    pub fn has_content(&self) -> bool {
        self.content.is_some()
    }

    /// Switch active tab: cache the outgoing content, restore the incoming from cache
    /// (None -> skeleton). A pointer swap — no parse, so it can't block the slide.
    pub fn set_active_tab(&mut self, i: usize) {
        self.activate(i);
    }

    /// Reset per-tab focus/scroll/hero state (focus returns to top on a tab switch).
    fn reset_layout(&mut self, rails: usize) {
        self.card_focus = vec![0; rails];
        self.anim_x = vec![0.0; rails];
        self.anim_y = 0.0;
        self.focus_rail = 0;
        self.hero_index = 0;
        self.hero_prev = 0;
        self.hero_anim = 1.0;
        self.hero_timer = 0.0;
    }

    fn activate(&mut self, i: usize) {
        if i == self.tab_active || i >= self.tab_count {
            return;
        }
        if self.tab_active < self.tabs_parsed.len() {
            self.tabs_parsed[self.tab_active] = self.content.take(); // cache outgoing
        }
        self.content = self.tabs_parsed.get_mut(i).and_then(|o| o.take()); // restore incoming
        self.tab_active = i;
        let rails = self.content.as_ref().map_or(0, |c| c.rails.len());
        self.reset_layout(rails);
    }

    pub fn summary(&self) -> String {
        match &self.content {
            None => "no content loaded".to_string(),
            Some(c) => {
                let items: usize = c.rails.iter().map(|r| r.items.len()).sum();
                let first = c.rails.first().map(|r| r.title.as_str()).unwrap_or("-");
                format!("{} rails, {} items; first rail = \"{}\"", c.rails.len(), items, first)
            }
        }
    }

    /// All *unique* poster URLs across every rail, so JS decodes each image once.
    pub fn all_poster_urls(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        if let Some(c) = &self.content {
            for rail in &c.rails {
                for item in &rail.items {
                    if seen.insert(item.poster.clone()) {
                        out.push(item.poster.clone());
                    }
                }
            }
        }
        out
    }

    /// Poster URLs near the focused rail that aren't on the GPU yet — the lazy-load
    /// queue JS should fetch. (We never fetch all 100 upfront.)
    pub fn posters_to_load(&self) -> Vec<String> {
        self.window_posters(LOAD_RADIUS)
            .into_iter()
            .filter(|u| !self.textures.contains_key(u))
            .collect()
    }

    /// LRU eviction: keep resident poster/backdrop textures bounded to MAX_POSTER_TEXTURES,
    /// removing the least-recently-used. The current window + hero backdrops are protected, so
    /// scrolling back to a still-resident rail shows instantly (no re-load).
    pub fn evict_lru(&mut self) {
        let total = self.textures.keys().filter(|k| k.starts_with("http")).count();
        if total <= MAX_POSTER_TEXTURES {
            return;
        }
        // Never evict what's currently near the viewport, or the hero backdrops.
        let mut protect = self.window_posters(LOAD_RADIUS);
        if let Some(c) = &self.content {
            for f in &c.featured {
                protect.insert(f.backdrop.clone());
            }
        }
        let mut evictable: Vec<(String, u64)> = self
            .textures
            .iter()
            .filter(|(k, _)| k.starts_with("http") && !protect.contains(k.as_str()))
            .map(|(k, t)| (k.clone(), t.used))
            .collect();
        evictable.sort_by_key(|(_, used)| *used); // oldest first
        let to_remove = total - MAX_POSTER_TEXTURES;
        for (k, _) in evictable.into_iter().take(to_remove) {
            if let Some(t) = self.textures.remove(&k) {
                unsafe { self.gl.delete_texture(t.texture) };
            }
        }
    }

    /// THE HANDOFF: upload JS-decoded RGBA pixels (a poster) to the GPU, keyed by URL.
    pub fn add_poster(
        &mut self,
        url: String,
        width: i32,
        height: i32,
        pixels: &[u8],
    ) -> Result<(), JsValue> {
        self.upload(url, width, height, pixels)
    }

    /// Upload a decoded ImageBitmap straight to a texture (no JS pixel readback / no big
    /// byte array). The texture is created/bound via glow; only the upload uses web-sys.
    pub fn add_poster_bitmap(
        &mut self,
        url: String,
        bitmap: &web_sys::ImageBitmap,
    ) -> Result<(), JsValue> {
        unsafe {
            if let Some(old) = self.textures.remove(&url) {
                self.gl.delete_texture(old.texture);
            }
            let texture = self.gl.create_texture().map_err(JsValue::from)?;
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.set_tex_params();
            // Same GL context as `gl`, so this uploads into the texture bound just above.
            self.webgl2
                .tex_image_2d_with_u32_and_u32_and_image_bitmap(
                    WebGl2RenderingContext::TEXTURE_2D,
                    0,
                    WebGl2RenderingContext::RGBA as i32,
                    WebGl2RenderingContext::RGBA,
                    WebGl2RenderingContext::UNSIGNED_BYTE,
                    bitmap,
                )?;
            self.textures.insert(
                url,
                Tex {
                    texture,
                    w: bitmap.width() as f32,
                    h: bitmap.height() as f32,
                    used: self.frame_seq,
                },
            );
        }
        Ok(())
    }

    /// Same handoff, for text rendered to a texture by JS (key e.g. "title:0").
    pub fn add_text_texture(
        &mut self,
        key: String,
        width: i32,
        height: i32,
        pixels: &[u8],
    ) -> Result<(), JsValue> {
        self.upload(key, width, height, pixels)
    }

    /// Rail titles, in order, so JS can render each to a text texture.
    pub fn rail_titles(&self) -> Vec<String> {
        self.content
            .as_ref()
            .map(|c| c.rails.iter().map(|r| r.title.clone()).collect())
            .unwrap_or_default()
    }

    // ---- pagination (simulated API) ----

    /// Append a fetched page of rails, extending the focus/scroll arrays so the new
    /// rails plug straight into the existing model.
    pub fn append_rails(&mut self, json: &str) -> Result<(), JsValue> {
        let page: RailsPage =
            serde_json::from_str(json).map_err(|e| JsValue::from(e.to_string()))?;
        if let Some(c) = &mut self.content {
            for rail in page.rails {
                c.rails.push(rail);
                self.card_focus.push(0);
                self.anim_x.push(0.0);
            }
        }
        Ok(())
    }

    /// How many rails are currently loaded (JS uses this to decide when to prefetch).
    pub fn loaded_rail_count(&self) -> usize {
        self.content.as_ref().map_or(0, |c| c.rails.len())
    }

    /// The focused rail index (JS uses this for the prefetch threshold).
    pub fn focus_rail(&self) -> usize {
        self.focus_rail
    }

    /// True while a scroll/slide is still animating — JS defers new decodes until false.
    pub fn scroll_busy(&self) -> bool {
        self.scroll_busy
    }

    // ---- top tab bar ----

    pub fn set_tab_count(&mut self, n: usize) {
        self.tab_count = n;
        self.tabs_parsed = (0..n).map(|_| None).collect();
    }
    pub fn tab_count(&self) -> usize {
        self.tab_count
    }
    /// Which tab is active (JS watches this to swap the tab's content).
    pub fn active_tab(&self) -> usize {
        self.tab_active
    }

    // ---- hero / featured carousel ----

    pub fn featured_count(&self) -> usize {
        self.content.as_ref().map_or(0, |c| c.featured.len())
    }

    /// Backdrop URLs for JS to load (eagerly — there are only a few).
    pub fn featured_backdrops(&self) -> Vec<String> {
        self.content
            .as_ref()
            .map(|c| c.featured.iter().map(|f| f.backdrop.clone()).collect())
            .unwrap_or_default()
    }

    /// Hero title/synopsis reuse the referenced item's strings.
    pub fn featured_title(&self, i: usize) -> String {
        self.featured_item(i).map(|it| it.title.clone()).unwrap_or_default()
    }
    pub fn featured_description(&self, i: usize) -> String {
        self.featured_item(i).map(|it| it.description.clone()).unwrap_or_default()
    }

    pub fn in_hero(&self) -> bool {
        self.zone == Zone::Hero
    }

    /// Point the focus at the current hero item's referenced (rail, card), so a
    /// following enter_meta() shows it via the normal Meta path.
    pub fn select_hero(&mut self) {
        if let Some((rail, card)) = self.hero_target() {
            self.focus_rail = rail;
            if let Some(slot) = self.card_focus.get_mut(rail) {
                *slot = card;
            }
        }
    }

    // ---- screen state machine (Home <-> Meta) ----

    pub fn is_home(&self) -> bool {
        self.screen == Screen::Home
    }

    /// Switch to the details screen. JS calls this AFTER rendering the meta text
    /// textures ("meta:title", "meta:desc") for the focused item.
    pub fn enter_meta(&mut self) {
        self.screen = Screen::Meta;
    }

    pub fn is_meta(&self) -> bool {
        self.screen == Screen::Meta
    }
    pub fn is_player(&self) -> bool {
        self.screen == Screen::Player
    }

    /// Meta -> Player. JS starts the <video> around this call.
    pub fn enter_player(&mut self) {
        self.screen = Screen::Player;
    }

    /// Back one level: Player -> Meta -> Home. Focus is never changed, so Home is
    /// restored exactly as left.
    pub fn back(&mut self) {
        self.screen = match self.screen {
            Screen::Player => Screen::Meta,
            Screen::Meta => Screen::Home,
            Screen::Home => Screen::Home,
        };
    }

    /// JS pushes the <video>'s playback state each frame while on Player.
    pub fn set_playback(&mut self, current: f32, duration: f32, paused: bool) {
        self.current = current;
        self.duration = duration;
        self.paused = paused;
    }

    // Focused item fields, so JS can render its text on demand.
    pub fn focused_title(&self) -> String {
        self.focused_item().map(|i| i.title.clone()).unwrap_or_default()
    }
    pub fn focused_description(&self) -> String {
        self.focused_item().map(|i| i.description.clone()).unwrap_or_default()
    }
    pub fn focused_poster(&self) -> String {
        self.focused_item().map(|i| i.poster.clone()).unwrap_or_default()
    }
    pub fn focused_video(&self) -> String {
        self.focused_item().map(|i| i.video.clone()).unwrap_or_default()
    }

    /// Create a GPU texture from RGBA pixels and cache it (with its size) under `key`.
    fn upload(&mut self, key: String, width: i32, height: i32, pixels: &[u8]) -> Result<(), JsValue> {
        unsafe {
            // Replacing a key (e.g. the per-tick fps readout) frees the old texture.
            if let Some(old) = self.textures.remove(&key) {
                self.gl.delete_texture(old.texture);
            }
            let texture = self.gl.create_texture().map_err(JsValue::from)?;
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            self.set_tex_params();
            self.gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA as i32,
                width,
                height,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                Some(pixels),
            );
            self.textures.insert(
                key,
                Tex { texture, w: width as f32, h: height as f32, used: self.frame_seq },
            );
        }
        Ok(())
    }

    /// Handle a key from JS: move focus, clamped at the edges. Left/Right move within
    /// the current rail (and are remembered per rail); Up/Down switch rails.
    pub fn input(&mut self, key: &str) {
        // Arrow navigation only applies on Home.
        if self.screen != Screen::Home {
            return;
        }
        let rail_count = self.content.as_ref().map_or(0, |c| c.rails.len());
        if rail_count == 0 {
            return;
        }

        // Tabs zone: Left/Right switch tab (JS swaps content); Down enters the content.
        if self.zone == Zone::Tabs {
            match key {
                "ArrowRight" => {
                    if self.tab_active + 1 < self.tab_count {
                        let to = self.tab_active + 1;
                        self.activate(to); // pointer swap (cached content or skeleton)
                        self.tab_slide = self.res.0; // new tab slides in from the right
                    }
                }
                "ArrowLeft" => {
                    if self.tab_active > 0 {
                        let to = self.tab_active - 1;
                        self.activate(to);
                        self.tab_slide = -self.res.0; // new tab slides in from the left
                    }
                }
                "ArrowDown" => {
                    self.zone = if self.has_hero() { Zone::Hero } else { Zone::Rails };
                }
                _ => {}
            }
            return;
        }

        // Hero zone: Left/Right cycle the carousel; Up to tabs; Down to rails.
        if self.zone == Zone::Hero {
            let n = self.featured_count();
            match key {
                "ArrowRight" if n > 0 => self.set_hero((self.hero_index + 1) % n, 1.0),
                "ArrowLeft" if n > 0 => self.set_hero((self.hero_index + n - 1) % n, -1.0),
                "ArrowDown" => self.zone = Zone::Rails,
                "ArrowUp" => self.zone = Zone::Tabs,
                _ => {}
            }
            return;
        }

        // Rails zone.
        let prev = (self.focus_rail, self.focus_card_index());
        match key {
            "ArrowRight" => {
                let n = self.rail_len(self.focus_rail);
                if let Some(fc) = self.card_focus.get_mut(self.focus_rail) {
                    if *fc + 1 < n {
                        *fc += 1;
                    }
                }
            }
            "ArrowLeft" => {
                if let Some(fc) = self.card_focus.get_mut(self.focus_rail) {
                    *fc = fc.saturating_sub(1);
                }
            }
            "ArrowDown" => {
                if self.focus_rail + 1 < rail_count {
                    self.focus_rail += 1;
                }
            }
            "ArrowUp" => {
                if self.focus_rail == 0 {
                    // back up into the hero, or the tab bar if this tab has no hero
                    self.zone = if self.has_hero() { Zone::Hero } else { Zone::Tabs };
                } else {
                    self.focus_rail -= 1;
                }
            }
            _ => {}
        }
        // Pop the scale only on a real move; at an edge nothing changes, so no blink.
        if (self.focus_rail, self.focus_card_index()) != prev {
            self.focus_scale = 1.0;
        }
    }

    /// Advance animations by `dt_ms` and draw. Called every frame by JS's rAF loop.
    pub fn tick(&mut self, dt_ms: f32) {
        // Stamp recency on the on-screen window so the LRU keeps what's near the viewport.
        self.frame_seq = self.frame_seq.wrapping_add(1);
        for url in self.window_posters(LOAD_RADIUS) {
            if let Some(t) = self.textures.get_mut(&url) {
                t.used = self.frame_seq;
            }
        }

        let ch = self.res.1;
        let (_, _, card_pitch, rail_pitch, _, margin_y) = self.metrics(ch);
        let hero_h = if self.has_hero() { ch * HERO_FRAC } else { 0.0 };

        // Auto-advance the hero carousel while it has focus on Home.
        let n = self.featured_count();
        if self.screen == Screen::Home && self.zone == Zone::Hero && n > 0 {
            self.hero_timer += dt_ms;
            if self.hero_timer >= HERO_AUTO_MS {
                self.set_hero((self.hero_index + 1) % n, 1.0); // auto = slide in from right
            }
        }

        // Exponential smoothing: cover a fraction `k` of the remaining distance this
        // frame. This is naturally ease-out and roughly frame-rate independent.
        let k = 1.0 - (-dt_ms / SMOOTH_TAU_MS).exp();

        // Vertical scroll target: Hero zone shows the top (hero); Rails zone scrolls
        // the hero away and pins the focused rail near the top.
        // Rails block_top sits at column (hero_h + margin_y + r*pitch); this target makes
        // the focused rail rest at screen y = margin_y (a small gap below the top).
        let target_y = match self.zone {
            Zone::Tabs | Zone::Hero => 0.0,
            Zone::Rails => hero_h + self.focus_rail as f32 * rail_pitch,
        };
        let _ = margin_y;
        self.anim_y += (target_y - self.anim_y) * k;

        for r in 0..self.anim_x.len() {
            let target_x = self.card_focus[r] as f32 * card_pitch;
            self.anim_x[r] += (target_x - self.anim_x[r]) * k;
        }

        // Focused card eases toward FOCUS_SCALE_MAX; input() resets it to 1.0 to re-pop.
        self.focus_scale += (FOCUS_SCALE_MAX - self.focus_scale) * k;

        // Hero slide progress eases toward 1 (settled), a touch slower than scroll.
        let ks = 1.0 - (-dt_ms / HERO_SLIDE_TAU_MS).exp();
        self.hero_anim += (1.0 - self.hero_anim) * ks;
        // Tab content slides toward 0 (settled) on a tab switch.
        self.tab_slide += (0.0 - self.tab_slide) * ks;

        // "Busy" while the vertical scroll or a tab slide is still far from settled —
        // JS uses this to defer new image decodes until the motion stops.
        self.scroll_busy =
            (target_y - self.anim_y).abs() > SETTLE_PX || self.tab_slide.abs() > SETTLE_PX;

        self.draw();
    }

    /// Layout metrics derived from canvas height — shared by tick (targets) and draw.
    /// Returns (card_w, card_h, card_pitch, rail_pitch, margin_x, margin_y).
    fn metrics(&self, ch: f32) -> (f32, f32, f32, f32, f32, f32) {
        let card_h = ch * 0.34;
        let card_w = card_h * 2.0 / 3.0; // posters are 2:3
        let card_pitch = card_w + 18.0; // + horizontal gap
        // Each rail block = title + gap + cards + vertical gap.
        let rail_pitch = TITLE_H + TITLE_GAP + card_h + 36.0;
        (card_w, card_h, card_pitch, rail_pitch, 64.0, 60.0)
    }

    /// Draw all rails using the *animated* scroll/scale values.
    fn draw(&self) {
        let (cw, ch) = self.res;
        // On Player the canvas must be transparent so the <video> behind shows through.
        let bg = if self.screen == Screen::Player {
            [0.0, 0.0, 0.0, 0.0]
        } else {
            [0.07, 0.07, 0.10, 1.0]
        };
        unsafe {
            self.gl.clear_color(bg[0], bg[1], bg[2], bg[3]);
            self.gl.clear(glow::COLOR_BUFFER_BIT);
            self.gl.use_program(Some(self.program));
            self.gl.uniform_2_f32(self.res_loc.as_ref(), cw, ch);
            self.gl.uniform_1_i32(self.tex_loc.as_ref(), 0);
        }
        match self.screen {
            Screen::Home => self.draw_home(),
            Screen::Meta => self.draw_meta(),
            Screen::Player => self.draw_player(),
        }

        // Frame-time readout, top-right, over every screen.
        if let Some(t) = self.textures.get("ui:fps") {
            let h = 48.0;
            let w = h * (t.w / t.h);
            self.blit_texture(cw - w - 28.0, 24.0, w, h, t.texture);
        }
    }

    /// Home screen: fixed tab bar on top, then (optional) hero banner, then rails.
    fn draw_home(&self) {
        let (cw, ch) = self.res;
        let content = match &self.content {
            Some(c) => c,
            None => {
                // Tab not loaded yet: show a placeholder skeleton (slides in with the tab).
                self.draw_skeleton(cw, ch, self.tab_slide);
                self.draw_tab_bar(cw);
                return;
            }
        };
        let (card_w, card_h, card_pitch, rail_pitch, margin_x, margin_y) = self.metrics(ch);
        let has_hero = !content.featured.is_empty();
        let hero_h = if has_hero { ch * HERO_FRAC } else { 0.0 };
        // Content column sits below the fixed tab bar; first rail title is here:
        let rails_origin = TAB_BAR_H + hero_h + margin_y;
        let in_rails = self.zone == Zone::Rails;
        let sx = self.tab_slide; // horizontal slide offset during a tab switch

        // The focused card is drawn LAST (on top of neighbours), so defer it.
        let mut focused_draw: Option<(f32, f32, Option<glow::Texture>)> = None;

        for (r, rail) in content.rails.iter().enumerate() {
            let block_top = rails_origin + r as f32 * rail_pitch - self.anim_y;
            let row_y = block_top + TITLE_H + TITLE_GAP; // cards sit below the title
            if row_y + card_h < 0.0 || block_top > ch {
                continue; // rail fully off-screen
            }

            if let Some(t) = self.textures.get(&format!("title:{}", r)) {
                let tw = TITLE_H * (t.w / t.h);
                self.blit_texture(margin_x + sx, block_top, tw, TITLE_H, t.texture);
            }

            let scroll_x = *self.anim_x.get(r).unwrap_or(&0.0);
            let rail_focus = *self.card_focus.get(r).unwrap_or(&0);

            for (c, item) in rail.items.iter().enumerate() {
                let x = margin_x + sx + c as f32 * card_pitch - scroll_x;
                if x + card_w < 0.0 || x > cw {
                    continue; // card off-screen
                }
                let tex = self.textures.get(&item.poster).map(|t| t.texture);
                if in_rails && r == self.focus_rail && c == rail_focus {
                    focused_draw = Some((x, row_y, tex)); // draw later, on top
                    continue;
                }
                self.draw_card(x, row_y, card_w, card_h, tex);
            }
        }

        // Focused card (rails zone): scaled up, drawn on top, slides into the frame.
        if let Some((x, row_y, tex)) = focused_draw {
            let s = self.focus_scale;
            let sw = card_w * s;
            let sh = card_h * s;
            self.draw_card(x + card_w / 2.0 - sw / 2.0, row_y + card_h / 2.0 - sh / 2.0, sw, sh, tex);
        }

        // Fixed focus frame (rails zone only) at the resting slot — rails slide under it.
        if in_rails {
            let s = self.focus_scale;
            let sw = card_w * s;
            let sh = card_h * s;
            let slot_y = TAB_BAR_H + margin_y + TITLE_H + TITLE_GAP;
            let fx = margin_x + sx + card_w / 2.0 - sw / 2.0;
            let fy = slot_y + card_h / 2.0 - sh / 2.0;
            self.draw_frame(fx, fy, sw, sh, 5.0, [0.30, 0.80, 1.0, 1.0]);
        }

        // Hero (if this tab has one), below the tab bar; drawn over rails sliding up.
        if has_hero {
            let hero_y = TAB_BAR_H - self.anim_y;
            if hero_y + hero_h > 0.0 && hero_y < ch {
                self.draw_hero(cw, hero_y, hero_h, sx);
            }
        }

        // Tab bar last — a fixed strip occluding anything scrolled up under it.
        self.draw_tab_bar(cw);
    }

    /// Hero banner with a slide transition: during a change the outgoing and incoming
    /// panels are both drawn, offset horizontally; the dots stay fixed on top.
    fn draw_hero(&self, cw: f32, y: f32, h: f32, base_x: f32) {
        if self.hero_anim < 0.999 {
            let rem = (1.0 - self.hero_anim) * cw; // distance still to travel
            let cur_x = base_x + self.hero_dir * rem; // incoming: off-screen -> base_x
            let prev_x = cur_x - self.hero_dir * cw; // outgoing: base_x -> off-screen
            self.draw_hero_panel(self.hero_prev, prev_x, cw, y, h);
            self.draw_hero_panel(self.hero_index, cur_x, cw, y, h);
        } else {
            self.draw_hero_panel(self.hero_index, base_x, cw, y, h);
        }
        self.draw_hero_dots(cw, y, h, base_x);
    }

    /// One featured panel (backdrop + scrim + title/synopsis/More-Info), shifted by x_off.
    fn draw_hero_panel(&self, index: usize, x_off: f32, cw: f32, y: f32, h: f32) {
        match self.backdrop_of(index).and_then(|u| self.textures.get(&u).map(|t| t.texture)) {
            Some(tex) => self.blit_texture(x_off, y, cw, h, tex),
            None => self.fill_rect(x_off, y, cw, h, [0.12, 0.12, 0.14, 1.0]),
        }
        self.fill_rect(x_off, y, cw, h, [0.0, 0.0, 0.0, 0.22]);
        self.fill_rect(x_off, y + h * 0.45, cw, h * 0.55, [0.0, 0.0, 0.0, 0.5]);

        let pad = cw * 0.06 + x_off;
        let mut ty = y + h * 0.30;

        if let Some(t) = self.textures.get(&format!("hero:title:{}", index)) {
            let mut th = 64.0;
            let max_w = cw * 0.6;
            let mut tw = th * (t.w / t.h);
            if tw > max_w {
                tw = max_w;
                th = tw * (t.h / t.w);
            }
            self.blit_texture(pad, ty, tw, th, t.texture);
            ty += th + 18.0;
        }
        if let Some(t) = self.textures.get(&format!("hero:desc:{}", index)) {
            let dw = (cw * 0.5).min(t.w);
            let dh = dw * (t.h / t.w);
            self.blit_texture(pad, ty, dw, dh, t.texture);
            ty += dh + 22.0;
        }
        let (bw, bh) = (220.0, 56.0);
        let col = if self.zone == Zone::Hero {
            [0.30, 0.80, 1.0, 1.0]
        } else {
            [1.0, 1.0, 1.0, 0.25]
        };
        self.fill_rect(pad, ty, bw, bh, col);
        if let Some(t) = self.textures.get("ui:moreinfo") {
            let lh = 24.0;
            let lw = lh * (t.w / t.h);
            self.blit_texture(pad + (bw - lw) / 2.0, ty + (bh - lh) / 2.0, lw, lh, t.texture);
        }
    }

    /// Carousel position dots (fixed; the active dot tracks hero_index).
    fn draw_hero_dots(&self, cw: f32, y: f32, h: f32, base_x: f32) {
        let pad = cw * 0.06 + base_x;
        let (dot, gap) = (12.0, 10.0);
        let dy = y + h - 42.0;
        for i in 0..self.featured_count() {
            let c = if i == self.hero_index {
                [1.0, 1.0, 1.0, 1.0]
            } else {
                [1.0, 1.0, 1.0, 0.35]
            };
            self.fill_rect(pad + i as f32 * (dot + gap), dy, dot, dot, c);
        }
    }

    /// Placeholder rows shown while a tab's data is still loading. No content needed.
    fn draw_skeleton(&self, cw: f32, ch: f32, sx: f32) {
        let (card_w, card_h, card_pitch, rail_pitch, margin_x, margin_y) = self.metrics(ch);
        let origin = TAB_BAR_H + margin_y;
        let mut r = 0;
        loop {
            let block_top = origin + r as f32 * rail_pitch;
            if block_top > ch {
                break;
            }
            // title bar placeholder
            self.fill_rect(margin_x + sx, block_top, 220.0, TITLE_H, [0.16, 0.16, 0.18, 1.0]);
            let row_y = block_top + TITLE_H + TITLE_GAP;
            let mut c = 0;
            loop {
                let x = margin_x + sx + c as f32 * card_pitch;
                if x > cw {
                    break;
                }
                self.fill_rect(x, row_y, card_w, card_h, [0.13, 0.13, 0.15, 1.0]);
                c += 1;
            }
            r += 1;
        }
    }

    /// The fixed top navigation bar: tab labels, the active tab shown in a white pill
    /// (dark label), the others as plain white labels.
    fn draw_tab_bar(&self, cw: f32) {
        self.fill_rect(0.0, 0.0, cw, TAB_BAR_H, [0.05, 0.05, 0.07, 1.0]);
        let label_h = 26.0;
        let y = (TAB_BAR_H - label_h) / 2.0;
        let padx = 22.0; // pill horizontal padding
        let gap = 26.0;
        let mut x = cw * 0.06;
        for i in 0..self.tab_count {
            let white = self.textures.get(&format!("tab:{}", i)).copied();
            let lw = white.map_or(70.0, |t| label_h * (t.w / t.h));
            if i == self.tab_active {
                self.fill_rect(x - padx, y - 9.0, lw + 2.0 * padx, label_h + 18.0, [1.0, 1.0, 1.0, 0.95]);
                if let Some(t) = self.textures.get(&format!("tabd:{}", i)) {
                    self.blit_texture(x, y, label_h * (t.w / t.h), label_h, t.texture);
                }
            } else if let Some(t) = white {
                self.blit_texture(x, y, lw, label_h, t.texture);
            }
            x += lw + 2.0 * padx + gap;
        }
    }

    /// Meta / details screen for the focused item: large poster left, text + Play right.
    fn draw_meta(&self) {
        let (cw, ch) = self.res;

        // Large poster on the left.
        let poster_h = ch * 0.62;
        let poster_w = poster_h * 2.0 / 3.0;
        let px = cw * 0.08;
        let py = (ch - poster_h) / 2.0;
        let tex = self.textures.get(&self.focused_poster()).map(|t| t.texture);
        self.draw_card(px, py, poster_w, poster_h, tex);

        // Text column on the right.
        let tx = px + poster_w + cw * 0.05;
        let mut ty = py + 6.0;

        // Title — shrink to fit the column if a long title would overflow.
        let max_w = cw - tx - cw * 0.05;
        if let Some(t) = self.textures.get("meta:title") {
            let mut th = 56.0;
            let mut tw = th * (t.w / t.h);
            if tw > max_w {
                tw = max_w;
                th = tw * (t.h / t.w);
            }
            self.blit_texture(tx, ty, tw, th, t.texture);
            ty += th + 28.0;
        }
        // Description (pre-wrapped paragraph texture). Don't upscale past its source.
        if let Some(t) = self.textures.get("meta:desc") {
            let dw = (cw - tx - cw * 0.08).min(t.w);
            let dh = dw * (t.h / t.w);
            self.blit_texture(tx, ty, dw, dh, t.texture);
            ty += dh + 34.0;
        }
        // Play button (the only focusable affordance on Meta, so always highlighted).
        let (bw, bh) = (200.0, 64.0);
        self.fill_rect(tx, ty, bw, bh, [0.30, 0.80, 1.0, 1.0]);
        if let Some(t) = self.textures.get("ui:play") {
            let lh = 28.0;
            let lw = lh * (t.w / t.h);
            self.blit_texture(tx + (bw - lw) / 2.0, ty + (bh - lh) / 2.0, lw, lh, t.texture);
        }
    }

    /// Player screen: the canvas is transparent (video plays behind); UI drawn on top.
    fn draw_player(&self) {
        let (cw, ch) = self.res;

        // Bottom scrim so the controls stay legible over any video frame.
        let scrim_h = ch * 0.22;
        self.fill_rect(0.0, ch - scrim_h, cw, scrim_h, [0.0, 0.0, 0.0, 0.55]);

        // Title, top-left (reusing the texture rendered when entering Meta).
        if let Some(t) = self.textures.get("meta:title") {
            let mut th = 44.0;
            let max_w = cw * 0.85;
            let mut tw = th * (t.w / t.h);
            if tw > max_w {
                tw = max_w;
                th = tw * (t.h / t.w);
            }
            self.blit_texture(cw * 0.06, ch * 0.08, tw, th, t.texture);
        }

        // Progress bar near the bottom, filled by currentTime / duration.
        let bx = cw * 0.06;
        let bw = cw * 0.88;
        let by = ch - scrim_h * 0.4;
        let bh = 6.0;
        self.fill_rect(bx, by, bw, bh, [1.0, 1.0, 1.0, 0.25]); // track
        let frac = if self.duration > 0.0 {
            (self.current / self.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        self.fill_rect(bx, by, bw * frac, bh, [0.30, 0.80, 1.0, 1.0]); // filled

        // Play/pause indicator above the bar.
        let s = 30.0;
        let iy = by - s - 18.0;
        if self.paused {
            // Play glyph (a text texture).
            if let Some(t) = self.textures.get("ui:icon_play") {
                self.blit_texture(bx, iy, s * (t.w / t.h), s, t.texture);
            }
        } else {
            // Pause = two bars, drawn with primitives (no glyph needed).
            let barw = s * 0.32;
            let gap = s * 0.30;
            self.fill_rect(bx, iy, barw, s, [1.0, 1.0, 1.0, 0.95]);
            self.fill_rect(bx + barw + gap, iy, barw, s, [1.0, 1.0, 1.0, 0.95]);
        }
    }

    /// Draw a poster if its texture is ready, else a light-grey placeholder
    /// (shown until the image finishes loading in the background).
    fn draw_card(&self, x: f32, y: f32, w: f32, h: f32, tex: Option<glow::Texture>) {
        match tex {
            Some(t) => self.blit_texture(x, y, w, h, t),
            None => self.fill_rect(x, y, w, h, [0.6, 0.6, 0.62, 1.0]),
        }
    }

    /// Draw a hollow rectangular outline (four thin bars) of thickness `t`.
    fn draw_frame(&self, x: f32, y: f32, w: f32, h: f32, t: f32, color: [f32; 4]) {
        self.fill_rect(x - t, y - t, w + 2.0 * t, t, color); // top
        self.fill_rect(x - t, y + h, w + 2.0 * t, t, color); // bottom
        self.fill_rect(x - t, y, t, h, color); // left
        self.fill_rect(x + w, y, t, h, color); // right
    }

    // ---- internal helpers ----

    /// The focused card index within the current rail.
    fn focus_card_index(&self) -> usize {
        self.card_focus.get(self.focus_rail).copied().unwrap_or(0)
    }

    /// The currently focused item, if any.
    fn focused_item(&self) -> Option<&Item> {
        let content = self.content.as_ref()?;
        let rail = content.rails.get(self.focus_rail)?;
        rail.items.get(self.focus_card_index())
    }

    /// Begin a slide to `new_index`; `dir` = +1 (incoming from right) or -1 (from left).
    fn set_hero(&mut self, new_index: usize, dir: f32) {
        self.hero_prev = self.hero_index;
        self.hero_index = new_index;
        self.hero_anim = 0.0;
        self.hero_dir = dir;
        self.hero_timer = 0.0;
    }

    /// The (rail, card) a hero item points at.
    fn hero_target(&self) -> Option<(usize, usize)> {
        let c = self.content.as_ref()?;
        let f = c.featured.get(self.hero_index)?;
        Some((f.rail, f.card))
    }

    /// The rail item a hero item references.
    fn featured_item(&self, i: usize) -> Option<&Item> {
        let c = self.content.as_ref()?;
        let f = c.featured.get(i)?;
        c.rails.get(f.rail)?.items.get(f.card)
    }

    /// Does the active tab have a hero banner? (true when it has featured items).
    fn has_hero(&self) -> bool {
        self.content.as_ref().map_or(false, |c| !c.featured.is_empty())
    }

    /// The backdrop URL for featured item `i`.
    fn backdrop_of(&self, i: usize) -> Option<String> {
        let c = self.content.as_ref()?;
        c.featured.get(i).map(|f| f.backdrop.clone())
    }

    /// Poster URLs in a 2D window around the focus: rails within `radius` of the focused
    /// rail, and within each, only cards within HSPAN of that rail's focused card.
    fn window_posters(&self, radius: usize) -> HashSet<String> {
        let mut set = HashSet::new();
        if let Some(c) = &self.content {
            if c.rails.is_empty() {
                return set;
            }
            let lo = self.focus_rail.saturating_sub(radius);
            let hi = (self.focus_rail + radius).min(c.rails.len() - 1);
            for r in lo..=hi {
                let items = &c.rails[r].items;
                if items.is_empty() {
                    continue;
                }
                let center = *self.card_focus.get(r).unwrap_or(&0);
                let clo = center.saturating_sub(HSPAN);
                let chi = (center + HSPAN).min(items.len() - 1);
                for item in &items[clo..=chi] {
                    set.insert(item.poster.clone());
                }
            }
        }
        set
    }

    fn rail_len(&self, rail: usize) -> usize {
        self.content
            .as_ref()
            .and_then(|c| c.rails.get(rail))
            .map_or(0, |r| r.items.len())
    }

    /// Draw a flat-colored rectangle (focus border / placeholder).
    fn fill_rect(&self, x: f32, y: f32, w: f32, h: f32, color: [f32; 4]) {
        unsafe {
            self.gl.uniform_1_i32(self.use_tex_loc.as_ref(), 0);
            self.gl
                .uniform_4_f32(self.color_loc.as_ref(), color[0], color[1], color[2], color[3]);
        }
        self.draw_rect(x, y, w, h);
    }

    /// Draw a textured rectangle (a poster).
    fn blit_texture(&self, x: f32, y: f32, w: f32, h: f32, texture: glow::Texture) {
        unsafe {
            self.gl.uniform_1_i32(self.use_tex_loc.as_ref(), 1);
            self.gl.active_texture(glow::TEXTURE0);
            self.gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        }
        self.draw_rect(x, y, w, h);
    }

    /// Upload one quad's geometry and draw it (shared by fill_rect / blit_texture).
    fn draw_rect(&self, x: f32, y: f32, w: f32, h: f32) {
        #[rustfmt::skip]
        let verts: [f32; 24] = [
            x,     y,     0.0, 0.0,
            x + w, y,     1.0, 0.0,
            x + w, y + h, 1.0, 1.0,
            x,     y,     0.0, 0.0,
            x + w, y + h, 1.0, 1.0,
            x,     y + h, 0.0, 1.0,
        ];
        unsafe {
            self.gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.vbo));
            self.gl
                .buffer_data_u8_slice(glow::ARRAY_BUFFER, as_bytes(&verts), glow::DYNAMIC_DRAW);
            self.gl.draw_arrays(glow::TRIANGLES, 0, 6);
        }
    }

    unsafe fn set_tex_params(&self) {
        let gl = &self.gl;
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
        gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
    }
}

fn as_bytes(floats: &[f32]) -> &[u8] {
    unsafe {
        core::slice::from_raw_parts(
            floats.as_ptr() as *const u8,
            floats.len() * core::mem::size_of::<f32>(),
        )
    }
}

unsafe fn link_program(
    gl: &glow::Context,
    vertex_src: &str,
    fragment_src: &str,
) -> Result<glow::Program, JsValue> {
    let vertex = compile_shader(gl, glow::VERTEX_SHADER, vertex_src)?;
    let fragment = compile_shader(gl, glow::FRAGMENT_SHADER, fragment_src)?;

    let program = gl.create_program().map_err(JsValue::from)?;
    gl.attach_shader(program, vertex);
    gl.attach_shader(program, fragment);
    gl.link_program(program);

    if !gl.get_program_link_status(program) {
        let log = gl.get_program_info_log(program);
        return Err(JsValue::from(format!("program link failed: {log}")));
    }

    gl.delete_shader(vertex);
    gl.delete_shader(fragment);
    Ok(program)
}

unsafe fn compile_shader(
    gl: &glow::Context,
    shader_type: u32,
    source: &str,
) -> Result<glow::Shader, JsValue> {
    let shader = gl.create_shader(shader_type).map_err(JsValue::from)?;
    gl.shader_source(shader, source);
    gl.compile_shader(shader);

    if !gl.get_shader_compile_status(shader) {
        let log = gl.get_shader_info_log(shader);
        return Err(JsValue::from(format!("shader compile failed: {log}")));
    }
    Ok(shader)
}
