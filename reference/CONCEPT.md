# Beisl — brand & app-icon concept

## The story we're encoding

A *Beisl* is the Viennese corner pub where the regulars loudly, precisely, and
without malice tell you what went wrong — and, just as loudly, when it didn't.
The tool wraps a game launch, samples where **wine** burns CPU, and hands you
the verdict by *layer*: kernel · GPU driver · game/JIT · DXVK/vkd3d · wine.
Three brand truths from DESIGN.md that the mark must carry:

1. **It vets the wine.** The name is a pun that must stay visible: a
   performance critic whose subject is literally wine.
2. **It's the honest regular, not a doom tool.** "Confirming a good game is
   good is a first-class result, not a null one." The mark should feel warm
   and convivial, not clinical or alarming.
3. **The product is the layer verdict.** The GUI's headline is a stacked
   colored bar — "who ate the frame." If the icon can *be* that answer, the
   icon and the app teach each other.

Family context: **SpritzWine** (the drink) · **Prater** (the funfair) ·
**Beisl** (the pub). Each sibling should own its own Viennese object.

---

## Concept A — "Das Glasl": the layer split, poured  → `icon-glass-pour.svg`

A stemmed wine glass in Kaffeehaus cream on a dark wood tile. The drink
inside is **the headline stacked bar, poured**: DXVK violet on top, game
amber in the middle, wine red settled at the bottom of the glass — the exact
hexes the results UI already uses (`chart.rs`). The surface of the pour is
not flat: it's the **frametime trace, with one hitch spike** — the meniscus
is the MangoHud line.

- **What it says:** "Look into the glass and see which layer filled it."
  Wine + verdict in one object. The spike whispers the side-channel
  (frametimes bookmark the hitches) to anyone who's used the app.
- **Why it's Beisl specifically:** it is not a generic wine glass — it's the
  app's own headline view *inside* the app's own name. Nobody else's wine
  glass has stratigraphy. And it stays in-family without stealing from the
  siblings: Spritz owns the drink recipe, Prater owns the wheel, Beisl owns
  the glass on the table.
- **Small-size legibility (verified by raster test at 22px):** the cream
  glass silhouette on the dark rounded tile is unmistakable in a taskbar;
  the three bands survive as three distinct colors at 22px (each band ≥4px
  in the 48 grid); the spike softens to a texture at 22px and comes back at
  32px+ — a detail that rewards zoom rather than a load-bearing element.
  The dark tile makes it theme-proof on light and dark shells.

## Concept B — "Der Deckel": the honest tab  → `icon-coaster-tab.svg`

A round Bierdeckel (coaster) with the classic double printed ring — in a
Beisl the waiter tallies your drinks in pen strokes on the coaster. Here the
four upright tally strokes are the layers (kernel slate, driver in the UI's
vendor-*neutral* teal — the icon, like the tool, is wine- and vendor-
agnostic and plays no favorites), and the **fifth stroke that closes the
tally — the one struck across the others — is wine red**.

- **What it says:** "Your frame, itemized." The pub's most honest artifact
  is the tab; Beisl's most honest artifact is the per-layer bill. The wine
  stroke crossing the others is the verdict being written down.
- **Why it's Beisl specifically:** the coaster tally is a real, specific
  piece of Wirtshaus culture — not a dashboard cliché — and "we tally what
  each layer consumed" is literally what the sampler does.
- **Small-size legibility:** the circle silhouette is excellent and unique
  next to square app tiles; at 22px the five strokes compress into a striped
  blur with a readable red slash — recognizable, but the *story* needs 32px+
  to land. Better as a secondary mark (coaster/sticker/README badge, where
  it's physically a coaster again) than as the taskbar icon.

## Concept C — the Riesenrad frametime gauge (considered, declined)

A Ferris wheel whose spokes double as a circular frametime gauge is a
handsome mark — but the **Riesenrad is Prater's name**. Spending Vienna's
single most iconic silhouette on the pub-named sibling would strip-mine the
family: when Prater wants its own mark, its obvious object is gone, and
users who know the family would misfile the icon. The wheel also reads as
"generic circular gauge" below 32px, which is exactly the speedometer-app
cliché to avoid. Declined for family reasons, not aesthetic ones.

---

## Recommendation: Concept A, with B as the secondary mark

**Ship "Das Glasl" (`icon-glass-pour.svg`) as the app icon**, with
`icon-symbolic.svg` as the monochrome/panel variant. Rationale:

- It's the only concept where **name, product, and picture are the same
  sentence**: Beisl (pub) serves wine (the subject) whose contents are the
  layer verdict (the product). Concept B tells the story; A *is* the UI.
- It verifies at 22px (raster-tested): strong cream-on-dark silhouette,
  three legible layer colors, no reliance on fine detail.
- It's cheerful. A poured glass says "sit down, let's look at it together" —
  the honest-regular energy — where a gauge or a red slash says "problem."
  A good run looks like a good pour; that's truth #2 in the mark itself.

Keep "Der Deckel" as the ecosystem's secondary asset: literal coasters for
conference swag, a README badge, the trace-library "tab" motif.

### Symbolic variant → `icon-symbolic.svg`

Single-color line art of the glass where the **liquid surface is the
frametime trace with one spike** — the meniscus-as-trace idea carried by
line alone. `currentColor`, so desktops and the libcosmic header can recolor
it; drop-in for `hicolor/symbolic/apps/beisl-symbolic.svg`.

---

## Wordmark & typography

Keep the existing lowercase **beisl** — lowercase is right: a Beisl doesn't
have a brass nameplate, it has a hand-painted sign. Direction:

- **Face:** a warm humanist-geometric sans, medium weight, slightly tight
  tracking. In-app (libcosmic) just use the platform stack (Open Sans/Inter)
  at weight 500 — no font shipping. For lockups/site, something with quiet
  Viennese-Modernist geometry: e.g. *Hanken Grotesk* or *Work Sans* medium.
- **The one flourish:** replace the tittle — the dot of the **i** — with a
  small **wine-red spike tick** (a tiny ▲ / rotated square in Beislrot).
  It's the hitch bookmark from the trace view, it's the only accent the
  wordmark needs, and it makes plain lowercase "beisl" ownable in one glyph.
- Never title-case it. "Beisl" only at sentence start in prose.

### Taglines (pick per surface)

| Line | Use |
|------|-----|
| **Who ate the frame?** | primary — it's DESIGN.md's own headline phrase |
| Your frame, itemized. | README subtitle, release notes |
| The honest regular. | about page / brand voice, needs context |
| Good runs are a result too. | empty-state / clean-run screen — truth #2 as copy |
| Na, wer war's? | Easter egg / de-AT locale only |

---

## Palette

Signature color: **Beislrot `#7A1F33`** — a deep house-pour burgundy.

The argument:

- **Kinship without collision.** It's visibly the darker vintage of the wine
  *layer* color `#9C2B3B` — same family, one shade down the cellar. That
  separation matters: the brand chrome (window accents, wordmark tick,
  coaster ring) must never be confusable with the wine *data* segment in a
  stacked bar, or every screenshot looks like the tool is blaming wine.
  Brand is chrome; `#9C2B3B` stays data ink.
- **Distinct from the vendor reds.** AMD `#ED1C24` and Qualcomm `#E4002B`
  are bright signal reds; `#7A1F33` is unambiguously darker and browner at
  any size, so a driver segment never reads as "brand."
- **Harmonizes with the existing chart set** — warm and low-chroma against
  slate `#5A6777`, violet `#8E6FD6`, amber `#E0A030`; it can sit next to all
  of them without vibrating.
- **It's the right *object* color:** not merlot-marketing red but the color
  of a house Zweigelt in a dim room — pub, not wine bar.

Full ladder:

| Token | Hex | Role |
|-------|-----|------|
| Beislrot | `#7A1F33` | signature: accents, wordmark tick, links (light theme) |
| Beislrot hell | `#B14358` | signature on dark themes (contrast-checked lift) |
| Kaffeehauscreme | `#F4EDE4` | glass, coaster stock, light surfaces |
| Wirtshausholz | `#2B2320` | icon tile, dark surfaces, footer |
| Layer/data colors | *(unchanged)* | `#5A6777` kernel · vendor/`#3A9188` driver · `#E0A030` game · `#8E6FD6` dxvk · `#9C2B3B` wine |

Rule of thumb: **Beislrot frames, layer colors testify.** The signature
color never appears inside a chart.

---

## Files

- `icon-glass-pour.svg` — recommended app icon (48 grid, flat, dark tile,
  theme-proof)
- `icon-coaster-tab.svg` — secondary mark / swag / badge
- `icon-symbolic.svg` — monochrome panel/symbolic variant (`currentColor`)

Install targets when adopted: `hicolor/scalable/apps/beisl.svg` (glass) and
`hicolor/symbolic/apps/beisl-symbolic.svg`; the `.desktop` file for
`beisl-setup` points at `Icon=beisl`.
