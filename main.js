// Thin JS glue. M1 adds the JS half of the image->texture handoff:
// decode an image into raw RGBA bytes that Rust can upload to the GPU.

import init, { Engine } from "./pkg/app.js";

// Decode an image URL into an ImageBitmap. Rust uploads it straight to a GPU texture
// (no 2D-canvas pixel readback, no ~0.5 MB byte array) — much cheaper on the main
// thread, which keeps frames steady while scrolling (especially on the TV).
async function decodeBitmap(url) {
  const blob = await (await fetch(url)).blob();
  return await createImageBitmap(blob);
}

// Render a text string onto an offscreen 2D canvas, then read back its RGBA pixels.
// This is how we get text into the engine without any DOM text — Rust uploads these
// pixels as a texture, exactly like a poster. Rendered large for crispness when scaled.
function makeTextTexture(text, { fontPx = 64, weight = "600", color = "#ffffff", pad = 6 } = {}) {
  const c = document.createElement("canvas");
  const ctx = c.getContext("2d");
  const fontStr = `${weight} ${fontPx}px sans-serif`;
  ctx.font = fontStr;
  const w = Math.ceil(ctx.measureText(text).width) + pad * 2;
  const h = Math.ceil(fontPx * 1.3) + pad * 2;
  c.width = w;
  c.height = h;
  ctx.font = fontStr; // resizing the canvas resets the context, so set it again
  ctx.textBaseline = "middle";
  ctx.fillStyle = color;
  ctx.fillText(text, pad, h / 2);
  const { data } = ctx.getImageData(0, 0, w, h);
  return { width: w, height: h, pixels: new Uint8Array(data.buffer) };
}

// Like makeTextTexture, but word-wraps `text` to `maxWidth` across multiple lines.
function makeParagraphTexture(text, { maxWidth = 640, fontPx = 34, lineHeight = 1.35, weight = "400", color = "#cfd2da", pad = 4 } = {}) {
  const fontStr = `${weight} ${fontPx}px sans-serif`;
  const measure = document.createElement("canvas").getContext("2d");
  measure.font = fontStr;

  const lines = [];
  let line = "";
  for (const word of text.split(/\s+/)) {
    const test = line ? `${line} ${word}` : word;
    if (line && measure.measureText(test).width > maxWidth) {
      lines.push(line);
      line = word;
    } else {
      line = test;
    }
  }
  if (line) lines.push(line);

  const lh = Math.round(fontPx * lineHeight);
  const w = maxWidth + pad * 2;
  const h = lines.length * lh + pad * 2;
  const c = document.createElement("canvas");
  c.width = w;
  c.height = h;
  const ctx = c.getContext("2d");
  ctx.font = fontStr;
  ctx.textBaseline = "top";
  ctx.fillStyle = color;
  lines.forEach((ln, i) => ctx.fillText(ln, pad, pad + i * lh));
  const { data } = ctx.getImageData(0, 0, w, h);
  return { width: w, height: h, pixels: new Uint8Array(data.buffer) };
}

async function run() {
  await init();
  const engine = Engine.new();

  const resize = () => engine.set_size(window.innerWidth, window.innerHeight);
  window.addEventListener("resize", resize); // the rAF loop redraws
  resize();

  // ---- Static UI textures (rendered once) ----
  {
    const p = makeTextTexture("▶  Play", { fontPx: 56 });
    engine.add_text_texture("ui:play", p.width, p.height, p.pixels);
    const ic = makeTextTexture("▶", { fontPx: 64 });
    engine.add_text_texture("ui:icon_play", ic.width, ic.height, ic.pixels);
    const mi = makeTextTexture("More Info", { fontPx: 44 });
    engine.add_text_texture("ui:moreinfo", mi.width, mi.height, mi.pixels);
  }

  // ---- Tabs: content.json is a manifest of tabs, each with its own data file ----
  const manifest = await (await fetch("content.json", { cache: "no-store" })).json();
  const tabs = manifest.tabs; // [{ name, file, hero }, ...]
  engine.set_tab_count(tabs.length);

  // Tab labels: white (inactive) + dark (shown in the active tab's white pill).
  tabs.forEach((tab, i) => {
    const w = makeTextTexture(tab.name, { fontPx: 40, weight: "600", color: "#ffffff" });
    engine.add_text_texture(`tab:${i}`, w.width, w.height, w.pixels);
    const d = makeTextTexture(tab.name, { fontPx: 40, weight: "700", color: "#0b0b0d" });
    engine.add_text_texture(`tabd:${i}`, d.width, d.height, d.pixels);
  });

  // Rail titles are rendered incrementally (a few per frame) to avoid a spike on tab
  // switch — rendering 20-25 text textures at once dropped a frame.
  let titleQueue = [];
  function queueRailTitles() {
    titleQueue = [];
    const n = engine.rail_titles().length;
    for (let i = 0; i < n; i++) titleQueue.push(i);
  }
  function drainTitles() {
    if (titleQueue.length === 0) return;
    const titles = engine.rail_titles();
    let k = 0;
    while (titleQueue.length > 0 && k < 4) {
      const i = titleQueue.shift();
      if (i < titles.length) {
        const t = makeTextTexture(titles[i]);
        engine.add_text_texture(`title:${i}`, t.width, t.height, t.pixels);
      }
      k++;
    }
  }

  // Render the active tab's hero assets (only if it has featured items).
  function renderHeroAssets() {
    for (let i = 0; i < engine.featured_count(); i++) {
      const ht = makeTextTexture(engine.featured_title(i), { fontPx: 96, weight: "700" });
      engine.add_text_texture(`hero:title:${i}`, ht.width, ht.height, ht.pixels);
      const hd = makeParagraphTexture(engine.featured_description(i), { maxWidth: 560, fontPx: 32, color: "#e6e8ee" });
      engine.add_text_texture(`hero:desc:${i}`, hd.width, hd.height, hd.pixels);
    }
    for (const url of engine.featured_backdrops()) {
      decodeBitmap(url)
        .then((bmp) => {
          engine.add_poster_bitmap(url, bmp);
          bmp.close();
        })
        .catch(() => {});
    }
  }

  // Lazy poster loading. Decoding happens async (any number in flight), but GPU UPLOADS
  // are queued and drained at most MAX_UPLOADS_PER_FRAME per frame — so tex_image_2d never
  // bursts and stretches a frame (the cause of FPS drops while scrolling on the TV).
  const MAX_UPLOADS_PER_FRAME = 2;
  const loadingPosters = new Set(); // urls being decoded or waiting to upload
  const uploadQueue = []; // decoded posters ready for the GPU: { url, width, height, pixels }

  function pumpPosters() {
    for (const url of engine.posters_to_load()) {
      if (loadingPosters.has(url)) continue;
      loadingPosters.add(url); // keep it claimed until uploaded (no duplicate fetches)
      decodeBitmap(url)
        .then((bmp) => uploadQueue.push({ url, bmp }))
        .catch(() => loadingPosters.delete(url));
    }
    engine.evict_lru(); // bounded LRU: keep recent textures, drop only the oldest over the cap
  }

  // Upload a few decoded posters per frame; the rest wait their turn.
  function drainUploads() {
    let n = 0;
    while (uploadQueue.length > 0 && n < MAX_UPLOADS_PER_FRAME) {
      const { url, bmp } = uploadQueue.shift();
      engine.add_poster_bitmap(url, bmp);
      bmp.close(); // release the decoded bitmap now it's on the GPU
      loadingPosters.delete(url);
      n++;
    }
  }

  // Tab data is fetched lazily on first visit and PARSED only once the slide settles (so the
  // parse never blocks the animation). Parsed tabs are memoized in the engine, so re-visiting
  // a tab is an instant pointer-swap — no re-fetch, no re-parse.
  let currentTab = 0;
  let renderedTab = -1; // which tab's titles/hero are currently rendered
  let heroAssetsDone = false;
  const tabFetched = new Set(); // tabs whose JSON fetch has started
  const pendingTexts = new Map(); // tab index -> fetched text awaiting parse (done when settled)

  function ensureTabFetched(i) {
    if (tabFetched.has(i)) return; // already fetched/parsing, or cached in the engine
    tabFetched.add(i);
    fetch(tabs[i].file, { cache: "no-store" })
      .then((r) => r.text())
      .then((text) => pendingTexts.set(i, text))
      .catch(() => tabFetched.delete(i));
  }
  ensureTabFetched(0); // first tab; parsed in the frame loop once settled
  console.log("[tabs]", tabs.map((t) => t.name).join(" / "));

  // Render the focused item's text and open the details (Meta) screen.
  function enterMeta() {
    const title = makeTextTexture(engine.focused_title(), { fontPx: 88 });
    engine.add_text_texture("meta:title", title.width, title.height, title.pixels);
    const desc = makeParagraphTexture(engine.focused_description(), { maxWidth: 640 });
    engine.add_text_texture("meta:desc", desc.width, desc.height, desc.pixels);
    engine.enter_meta();
  }

  // "More Info" from the hero: point focus at the featured item's (rail, card),
  // make sure its poster is loaded, then open Meta via the normal path.
  function enterMetaFromHero() {
    engine.select_hero();
    const purl = engine.focused_poster();
    if (purl) {
      decodeBitmap(purl)
        .then((bmp) => {
          engine.add_poster_bitmap(purl, bmp);
          bmp.close();
        })
        .catch(() => {});
    }
    enterMeta();
  }

  // ---- Player: drive the native <video> element. Video is the one thing the browser
  // handles itself; Rust just draws the overlay on top of the transparent canvas. ----
  const video = document.getElementById("video");

  function enterPlayer() {
    video.src = engine.focused_video();
    video.style.display = "block";
    // Real Enter is a user gesture, so audio plays. If autoplay is blocked (e.g. a
    // synthetic event), retry muted so the video still shows.
    video.play().catch(() => {
      video.muted = true;
      video.play().catch(() => {});
    });
    engine.enter_player();
  }

  function stopPlayback() {
    video.pause();
    video.removeAttribute("src");
    video.load();
    video.style.display = "none";
  }

  function togglePlay() {
    if (video.paused) video.play().catch(() => {});
    else video.pause();
  }

  // Exit the app (Tizen TV only; no-op in a normal browser).
  function exitApp() {
    try {
      if (typeof tizen !== "undefined" && tizen.application) {
        tizen.application.getCurrentApplication().exit();
      }
    } catch (_) {
      /* ignore */
    }
  }

  // Go back one level: Player -> Meta -> Home; on Home (top level) exit the app.
  // Shared by laptop Esc and TV remote Back.
  function goBack() {
    if (engine.is_home()) {
      exitApp(); // already at the top
      return;
    }
    if (engine.is_player()) stopPlayback(); // stop video, then drop back to Meta
    engine.back();
  }

  // Route keys: arrows navigate; Enter selects/plays; Back goes up a level.
  window.addEventListener("keydown", (e) => {
    // Samsung TV remote Back sends keyCode 10009 (not a named e.key) — handle it first.
    if (e.keyCode === 10009) {
      e.preventDefault();
      goBack();
      return;
    }
    switch (e.key) {
      case "ArrowLeft":
      case "ArrowRight":
      case "ArrowUp":
      case "ArrowDown":
        e.preventDefault();
        engine.input(e.key); // ignored by Rust unless on Home
        // The tab bar moved the active tab (Rust already swapped to cached content or a
        // skeleton). Kick off its fetch if needed; titles render once loaded + settled.
        if (engine.is_home() && engine.active_tab() !== currentTab) {
          currentTab = engine.active_tab();
          ensureTabFetched(currentTab);
        }
        break;
      case "Enter":
        e.preventDefault();
        if (engine.is_home()) {
          if (engine.in_hero()) enterMetaFromHero();
          else enterMeta();
        } else if (engine.is_meta()) enterPlayer();
        else if (engine.is_player()) togglePlay(); // Enter toggles play/pause
        break;
      case "Escape":
      case "Backspace":
        e.preventDefault();
        goBack(); // laptop back
        break;
    }
  });

  // Start the animation loop NOW — before images load. Cards with no texture yet
  // draw as light-grey placeholders; the loop redraws every frame.
  let last = performance.now();
  let fpsMs = 0; // accumulated frame interval (cadence)
  let workMs = 0; // accumulated tick() work time (render cost)
  let fpsN = 0;
  let fpsClock = last;
  let pumpClock = last;
  function frame(now) {
    const dt = now - last;
    last = now;

    // While playing, feed the video's clock into the engine so it can draw progress.
    if (engine.is_player()) {
      engine.set_playback(video.currentTime || 0, video.duration || 0, video.paused);
    }
    // Time the actual render work: how long tick() (Wasm + issuing the GL calls) takes.
    const t0 = performance.now();
    engine.tick(dt);
    workMs += performance.now() - t0;

    // Readout, refreshed ~3x/sec: fps, frame interval (cadence), and render work time.
    fpsMs += dt;
    fpsN++;
    if (now - fpsClock > 300) {
      const avgFrame = fpsMs / fpsN;
      const avgWork = workMs / fpsN;
      const label = `${Math.round(1000 / avgFrame)} fps   ${avgFrame.toFixed(1)} ms   work ${avgWork.toFixed(1)} ms`;
      const t = makeTextTexture(label, { fontPx: 72, weight: "500", color: "#8fe3ff" });
      engine.add_text_texture("ui:fps", t.width, t.height, t.pixels);
      fpsMs = 0;
      workMs = 0;
      fpsN = 0;
      fpsClock = now;
    }

    // All texture work happens ONLY when motion has settled, so scrolling and the tab
    // slide stay smooth (pure animation); content fills in the moment movement stops.
    if (!engine.scroll_busy()) {
      // Parse any fetched-but-unparsed tab data now (off the slide, so no pause).
      if (pendingTexts.size > 0) {
        for (const [i, text] of pendingTexts) engine.load_tab(i, text);
        pendingTexts.clear();
      }
      // Once the active tab has content, (re)render its titles + hero assets.
      if (renderedTab !== engine.active_tab() && engine.has_content()) {
        queueRailTitles();
        heroAssetsDone = false;
        renderedTab = engine.active_tab();
      }
      drainUploads();
      drainTitles();
      if (!heroAssetsDone && engine.has_content()) {
        renderHeroAssets();
        heroAssetsDone = true;
      }
      if (now - pumpClock > 150) {
        pumpPosters();
        pumpClock = now;
      }
    }

    requestAnimationFrame(frame);
  }
  requestAnimationFrame(frame);
}

run();
