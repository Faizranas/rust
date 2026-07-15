/* tslint:disable */
/* eslint-disable */
/**
*/
export class Engine {
  free(): void;
/**
* @returns {Engine}
*/
  static new(): Engine;
/**
* @param {number} width
* @param {number} height
*/
  set_size(width: number, height: number): void;
/**
* Parse a tab's JSON into the per-tab cache. If it's the active tab, make it live.
* Called by JS only when the slide has settled, so the parse never blocks the slide.
* @param {number} i
* @param {string} json
*/
  load_tab(i: number, json: string): void;
/**
* Is the active tab's content loaded (vs. showing a skeleton)?
* @returns {boolean}
*/
  has_content(): boolean;
/**
* Switch active tab: cache the outgoing content, restore the incoming from cache
* (None -> skeleton). A pointer swap — no parse, so it can't block the slide.
* @param {number} i
*/
  set_active_tab(i: number): void;
/**
* @returns {string}
*/
  summary(): string;
/**
* All *unique* poster URLs across every rail, so JS decodes each image once.
* @returns {(string)[]}
*/
  all_poster_urls(): (string)[];
/**
* Poster URLs near the focused rail that aren't on the GPU yet — the lazy-load
* queue JS should fetch. (We never fetch all 100 upfront.)
* @returns {(string)[]}
*/
  posters_to_load(): (string)[];
/**
* LRU eviction: keep resident poster/backdrop textures bounded to MAX_POSTER_TEXTURES,
* removing the least-recently-used. The current window + hero backdrops are protected, so
* scrolling back to a still-resident rail shows instantly (no re-load).
*/
  evict_lru(): void;
/**
* THE HANDOFF: upload JS-decoded RGBA pixels (a poster) to the GPU, keyed by URL.
* @param {string} url
* @param {number} width
* @param {number} height
* @param {Uint8Array} pixels
*/
  add_poster(url: string, width: number, height: number, pixels: Uint8Array): void;
/**
* Upload a decoded ImageBitmap straight to a texture (no JS pixel readback / no big
* byte array). The texture is created/bound via glow; only the upload uses web-sys.
* @param {string} url
* @param {ImageBitmap} bitmap
*/
  add_poster_bitmap(url: string, bitmap: ImageBitmap): void;
/**
* Same handoff, for text rendered to a texture by JS (key e.g. "title:0").
* @param {string} key
* @param {number} width
* @param {number} height
* @param {Uint8Array} pixels
*/
  add_text_texture(key: string, width: number, height: number, pixels: Uint8Array): void;
/**
* Rail titles, in order, so JS can render each to a text texture.
* @returns {(string)[]}
*/
  rail_titles(): (string)[];
/**
* Append a fetched page of rails, extending the focus/scroll arrays so the new
* rails plug straight into the existing model.
* @param {string} json
*/
  append_rails(json: string): void;
/**
* How many rails are currently loaded (JS uses this to decide when to prefetch).
* @returns {number}
*/
  loaded_rail_count(): number;
/**
* The focused rail index (JS uses this for the prefetch threshold).
* @returns {number}
*/
  focus_rail(): number;
/**
* True while a scroll/slide is still animating — JS defers new decodes until false.
* @returns {boolean}
*/
  scroll_busy(): boolean;
/**
* @param {number} n
*/
  set_tab_count(n: number): void;
/**
* @returns {number}
*/
  tab_count(): number;
/**
* Which tab is active (JS watches this to swap the tab's content).
* @returns {number}
*/
  active_tab(): number;
/**
* @returns {number}
*/
  featured_count(): number;
/**
* Backdrop URLs for JS to load (eagerly — there are only a few).
* @returns {(string)[]}
*/
  featured_backdrops(): (string)[];
/**
* Hero title/synopsis reuse the referenced item's strings.
* @param {number} i
* @returns {string}
*/
  featured_title(i: number): string;
/**
* @param {number} i
* @returns {string}
*/
  featured_description(i: number): string;
/**
* @returns {boolean}
*/
  in_hero(): boolean;
/**
* Point the focus at the current hero item's referenced (rail, card), so a
* following enter_meta() shows it via the normal Meta path.
*/
  select_hero(): void;
/**
* @returns {boolean}
*/
  is_home(): boolean;
/**
* Switch to the details screen. JS calls this AFTER rendering the meta text
* textures ("meta:title", "meta:desc") for the focused item.
*/
  enter_meta(): void;
/**
* @returns {boolean}
*/
  is_meta(): boolean;
/**
* @returns {boolean}
*/
  is_player(): boolean;
/**
* Meta -> Player. JS starts the <video> around this call.
*/
  enter_player(): void;
/**
* Back one level: Player -> Meta -> Home. Focus is never changed, so Home is
* restored exactly as left.
*/
  back(): void;
/**
* JS pushes the <video>'s playback state each frame while on Player.
* @param {number} current
* @param {number} duration
* @param {boolean} paused
*/
  set_playback(current: number, duration: number, paused: boolean): void;
/**
* @returns {string}
*/
  focused_title(): string;
/**
* @returns {string}
*/
  focused_description(): string;
/**
* @returns {string}
*/
  focused_poster(): string;
/**
* @returns {string}
*/
  focused_video(): string;
/**
* Handle a key from JS: move focus, clamped at the edges. Left/Right move within
* the current rail (and are remembered per rail); Up/Down switch rails.
* @param {string} key
*/
  input(key: string): void;
/**
* Advance animations by `dt_ms` and draw. Called every frame by JS's rAF loop.
* @param {number} dt_ms
*/
  tick(dt_ms: number): void;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
  readonly memory: WebAssembly.Memory;
  readonly __wbg_engine_free: (a: number) => void;
  readonly engine_new: (a: number) => void;
  readonly engine_set_size: (a: number, b: number, c: number) => void;
  readonly engine_load_tab: (a: number, b: number, c: number, d: number, e: number) => void;
  readonly engine_has_content: (a: number) => number;
  readonly engine_set_active_tab: (a: number, b: number) => void;
  readonly engine_summary: (a: number, b: number) => void;
  readonly engine_all_poster_urls: (a: number, b: number) => void;
  readonly engine_posters_to_load: (a: number, b: number) => void;
  readonly engine_evict_lru: (a: number) => void;
  readonly engine_add_poster: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
  readonly engine_add_poster_bitmap: (a: number, b: number, c: number, d: number, e: number) => void;
  readonly engine_rail_titles: (a: number, b: number) => void;
  readonly engine_append_rails: (a: number, b: number, c: number, d: number) => void;
  readonly engine_loaded_rail_count: (a: number) => number;
  readonly engine_focus_rail: (a: number) => number;
  readonly engine_scroll_busy: (a: number) => number;
  readonly engine_set_tab_count: (a: number, b: number) => void;
  readonly engine_tab_count: (a: number) => number;
  readonly engine_active_tab: (a: number) => number;
  readonly engine_featured_count: (a: number) => number;
  readonly engine_featured_backdrops: (a: number, b: number) => void;
  readonly engine_featured_title: (a: number, b: number, c: number) => void;
  readonly engine_featured_description: (a: number, b: number, c: number) => void;
  readonly engine_in_hero: (a: number) => number;
  readonly engine_select_hero: (a: number) => void;
  readonly engine_is_home: (a: number) => number;
  readonly engine_enter_meta: (a: number) => void;
  readonly engine_is_meta: (a: number) => number;
  readonly engine_is_player: (a: number) => number;
  readonly engine_enter_player: (a: number) => void;
  readonly engine_back: (a: number) => void;
  readonly engine_set_playback: (a: number, b: number, c: number, d: number) => void;
  readonly engine_focused_title: (a: number, b: number) => void;
  readonly engine_focused_description: (a: number, b: number) => void;
  readonly engine_focused_poster: (a: number, b: number) => void;
  readonly engine_focused_video: (a: number, b: number) => void;
  readonly engine_input: (a: number, b: number, c: number) => void;
  readonly engine_tick: (a: number, b: number) => void;
  readonly engine_add_text_texture: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => void;
  readonly __wbindgen_malloc: (a: number, b: number) => number;
  readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
  readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
  readonly __wbindgen_free: (a: number, b: number, c: number) => void;
  readonly __wbindgen_exn_store: (a: number) => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;
/**
* Instantiates the given `module`, which can either be bytes or
* a precompiled `WebAssembly.Module`.
*
* @param {SyncInitInput} module
*
* @returns {InitOutput}
*/
export function initSync(module: SyncInitInput): InitOutput;

/**
* If `module_or_path` is {RequestInfo} or {URL}, makes a request and
* for everything else, calls `WebAssembly.instantiate` directly.
*
* @param {InitInput | Promise<InitInput>} module_or_path
*
* @returns {Promise<InitOutput>}
*/
export default function __wbg_init (module_or_path?: InitInput | Promise<InitInput>): Promise<InitOutput>;
