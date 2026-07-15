# PRD — Rust/Wasm Streaming UI Demo ("Amazon-style" renderer)

**Owner:** Faiz
**Status:** Draft for build
**Goal in one line:** Build a small streaming UI where Rust (compiled to WebAssembly) does *all* layout, animation, and rendering to a single WebGL canvas — no DOM — to experience the Prime Video approach firsthand.

---

## 1. Objective

Reproduce, at demo scale, the architecture Prime Video ships today: a single app whose entire UI is written in Rust, compiled to Wasm, and drawn through a custom renderer onto one canvas — bypassing the browser DOM. The purpose is to *feel and inspect* this stack end to end, not to ship a product.

**What success looks like:** a running web app showing a Home screen of 10 poster rails navigable by d-pad/arrow keys with smooth slide animation, drilling into a details page and a player that plays an mp4 in a native `<video>` — with the entire UI (including the player overlay) rendered from Rust, zero DOM nodes for UI.

---

## 2. Scope

**In scope**
- Home screen: **10 horizontal rails** of poster cards.
- D-pad navigation (arrow keys + Enter + Back), focus highlight, rail slide animation, vertical rail-to-rail movement.
- Meta/details page: opens on Enter over a card; shows poster, title, description, a "Play" action.
- Player page: native **HTML5 `<video>`** playing an **mp4** URL, with the Rust/canvas layer rendering the player UI (progress, play/pause, metadata) on top.
- All UI rendered by Rust → WebGL canvas; **video is the one exception** (browser media pipeline).
- Content from a **hardcoded `content.json`** loaded via `fetch()` at runtime.

**Out of scope (this phase)**
- DRM and adaptive streaming (Shaka Player + HLS/m3u8) — planned next phase, see §12.
- AVPlay / platform-native players.
- Real API integration (the JSON is hardcoded, just loaded over fetch instead of embedded).
- Tizen/webOS packaging and on-TV deployment (desktop Chrome target for now).
- Multi-device capability detection / fallback paths.
- Accessibility, i18n, settings, search.
- **No React** — UI logic and rendering live entirely in Rust; only thin Vanilla JS glue exists.

---

## 3. Architecture

```
┌──────────────────────────────────────────────┐
│  Browser tab (desktop Chrome)                  │
│                                                │
│  ┌──────────┐   keydown      ┌──────────────┐  │
│  │ JS glue  │ ─────────────► │  Rust / Wasm │  │
│  │ (thin)   │                │   engine     │  │
│  │          │ ◄───────────── │              │  │
│  │ - rAF    │  draw commands │ - state M/C  │  │
│  │ - input  │   (via WebGL)  │ - layout     │  │
│  │ - image  │                │ - animation  │  │
│  │   decode │                │ - scene/draw │  │
│  │ - fetch  │                │ - player UI  │  │
│  │ - video  │                └──────────────┘  │
│  │   ctrl   │                       │           │
│  └────┬─────┘                       │           │
│       │ textures / play             ▼           │
│       ▼                  ┌────────────────────┐ │
│  ┌─────────────┐         │  <canvas> WebGL2   │ │
│  │  <video>    │ ◄ layered ► (UI overlay on   │ │
│  │  (mp4)      │         │   top of video)    │ │
│  └─────────────┘         └────────────────────┘ │
└──────────────────────────────────────────────┘
```

**Division of labour**
- **Rust/Wasm owns:** app state machine (Home/Meta/Player), layout math, focus model, animation/easing, the scene, and issuing WebGL draw calls. It also draws the *player UI* on top of the video.
- **JS glue (kept as thin as possible) owns only what Wasm can't reach directly:** the `requestAnimationFrame` loop, capturing keyboard events and forwarding them to Rust, decoding poster images into pixels the engine uploads as GL textures, `fetch()`-ing `content.json`, and controlling the HTML5 `<video>` element (load/play/pause/seek).
- **Video** plays in a native `<video>` element layered with the canvas (video behind, canvas UI overlaid on top). Video frames go through the browser's media pipeline, **not** WebGL.

This mirrors the Prime Video split: business/UI logic and rendering in Rust/Wasm; a minimal native/JS shell for the things only the host can do (media playback included).

---

## 4. Tech stack

| Concern | Choice |
|---|---|
| Language | Rust (stable) |
| Build | `wasm-pack` (uses `wasm-bindgen` under the hood) |
| Host bindings | `web-sys` / `js-sys` (canvas, WebGL2, events, rAF, image, video, fetch) |
| Graphics | WebGL2 via the `glow` crate + a hand-written sprite batcher |
| Data parsing | `serde` + `serde_json` (parse fetched JSON) |
| Data loading | `fetch()` of `content.json` (via JS glue / `web-sys`) |
| Video | native HTML5 `<video>`, mp4 source (demo); Shaka + HLS deferred to §12 |
| Dev server | Vite (or trunk) serving the Wasm bundle + JS glue |

> Versions pinned at kickoff to latest stable. `glow` chosen over `wgpu` for a lighter compile and a simpler, more transparent learning path for a first sprite renderer; `wgpu` is the upgrade path if we later want WebGPU.

---

## 5. Screens & behaviour

**Home**
- **10 rails**, each ~10 poster cards.
- One card focused at a time; focus drawn with a scale-up + border/glow.
- Left/Right: move focus within a rail; rail slides so focus stays in view (ease-out cubic, ~250ms).
- Up/Down: move between rails (vertical slide); the rail grid scrolls vertically so the focused rail stays in view.
- Enter: open Meta for the focused card.

**Meta / details**
- Large poster/backdrop, title, description text, a "Play" affordance.
- Enter on Play → Player. Back → Home (restores prior focus).

**Player**
- Native HTML5 `<video>` plays the item's **mp4** URL, layered behind the canvas.
- Rust/canvas renders the player UI on top: progress bar (driven by the video's `currentTime`/`duration`), play/pause indicator, title.
- Enter toggles play/pause (JS glue calls `video.play()`/`video.pause()`); Back stops playback and returns to Meta.

---

## 6. Data model (hardcoded JSON, loaded via fetch)

```json
{
  "rails": [
    {
      "title": "Trending Now",
      "items": [
        { "id": "t1", "title": "Title One",
          "poster": "https://.../p1.jpg",
          "video": "https://.../t1.mp4",
          "description": "Short synopsis text." }
      ]
    }
  ]
}
```

A single `content.json` with **10 rails** (~10 items each) is served as a static asset and loaded at runtime via `fetch()` (done by JS glue), then parsed in Rust with `serde`. Each item carries a `poster` (image URL) and a `video` (mp4 URL). Poster and video URLs point to any reachable host (or local assets).

---

## 7. Rust engine responsibilities (detail)

1. **State machine** — `Screen { Home, Meta(itemId), Player(itemId) }`; transitions on input.
2. **Data** — request `content.json` (JS glue fetches), parse with `serde` into rails/items.
3. **Layout** — compute card rects per rail from index, card size, gap, horizontal scroll offset, and vertical rail offset.
4. **Focus model** — track `(railIndex, cardIndex)`; clamp at edges.
5. **Animation** — a frame clock; tween horizontal/vertical scroll offsets and focus scale with easing.
6. **Renderer** — a sprite batcher: upload poster textures once, draw quads each frame with current transforms; draw focus highlight and text (bitmap font or pre-rendered text textures).
7. **Player UI** — read `currentTime`/`duration` from the video element (via glue) and draw progress + play/pause over the video layer.
8. **Input handling** — receive key codes from JS glue, mutate state; on Player, drive `<video>` play/pause through glue.

---

## 8. Build & run pipeline

1. `wasm-pack build` → produces the Wasm bundle + JS bindings.
2. Vite serves `index.html` (one `<canvas>` + one hidden `<video>`), the JS glue, the Wasm bundle, and `content.json`.
3. JS glue: init Wasm, grab canvas + WebGL2 context, `fetch('content.json')` and pass to Rust, start rAF loop calling the engine's `tick(dt)`, forward `keydown` events, decode images on demand and hand pixels to the engine, and drive the `<video>` element (show/hide, load src, play/pause) on Rust's instruction.

---

## 9. Milestones

| # | Milestone | Outcome |
|---|---|---|
| M0 | Toolchain + hello-triangle | Rust/Wasm draws one quad to canvas via WebGL2. |
| M1 | Sprite batcher + textures | Posters render as a static grid (image→bitmap→GL handoff proven). |
| M2 | Fetch + data model | `content.json` loaded via fetch, parsed in Rust, 10 rails built. |
| M3 | Rails + focus + input | D-pad navigation across 10 rails with focus highlight. |
| M4 | Animation | Smooth horizontal/vertical slide + focus scale. |
| M5 | Meta page | Card → details, back restores focus. |
| M6 | Player (HTML5 video) | mp4 plays in `<video>`; Rust draws progress + play/pause on top. |
| M7 | Polish + capture | Frame timing readout; lazy poster loading; record a demo clip. |

---

## 10. Success criteria

- Entire visible UI is rendered from Rust to one canvas; **no DOM elements used for UI.**
- Navigation feels responsive; slide animation is smooth on desktop Chrome.
- A simple on-screen frame-time counter is available for inspection.
- Codebase is small and readable enough to explain the architecture to the team.

---

## 11. Risks / open items

- **Text rendering in Rust/WebGL** is the fiddliest piece (no DOM text). Mitigation: start with a bitmap font or pre-rendered text-as-texture; keep copy minimal.
- **Image decode path** must go through browser APIs (JS glue decodes via `createImageBitmap`), then upload to GL with `texImage2D` — confirm the handoff early (folded into M1).
- **Texture memory at 10 rails (~100 posters).** Decoding and holding ~100 textures is fine on desktop but will pressure GPU memory on a TV. Mitigation (M7): lazy-load — only decode/upload posters near the viewport, free textures for off-screen rails.
- **Video + canvas layering.** The `<video>` sits behind the canvas; the canvas must be transparent where video shows through. Confirm compositing and that play/pause via glue stays in sync with the Rust UI state.
- **Compile times / bundle size** for Rust→Wasm — acceptable for a demo; revisit if it drags.
- **Deferred:** Chrome 76 / Tizen / webOS behaviour, DRM + adaptive streaming (§12), real API — all intentionally out for now, to be reintroduced once the core experience is validated on desktop.

---

## 12. Future phase — DRM & adaptive streaming

Once the demo experience is validated, the player evolves from bare mp4 to production-grade streaming:

- **Shaka Player** drives the HTML5 `<video>` element for adaptive streaming and DRM.
- **HLS (m3u8)** and **DASH** manifests instead of single mp4 files.
- **Widevine** (and platform DRM) for protected content — ties into the existing Tizen/Widevine L1 work.
- The architecture doesn't change: video still plays in the `<video>` layer through the browser/platform media pipeline; Shaka replaces the bare `src=mp4`. The Rust/canvas UI overlay is unaffected.
- Reintroduce the **Tizen/webOS packaging** and **Chrome 76 capability testing** in this phase, since DRM and older-engine behaviour are platform-specific.
