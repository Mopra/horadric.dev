# GPU light: every glow, shadow and spark drawn the way a game draws them

Proposed 2026-10-31. The first piece is built: a working session's lamp
surges with real bloom (`render/bloom.rs`, commit `11fe3c1`, the lore
has how). The human looked at it and called it much better. This part
takes the rest of Horadric the same way.

## Why

Every soft thing in Horadric is faked by drawing a hard shape many
times. A glow is a radial gradient (`glow_dot`) or five stacked rings
(`halo`, the lamp's spill). A shadow is eight rounded rectangles, each a
little bigger and fainter (`cast`, `BLUR_STEPS`). Up close, and next to
the Folio, these read as stickers: the edges band, light never adds up
where two glows meet, and nothing saturates toward white the way a
bright thing does. The human's words: it should be game engine quality,
not small gradients stacked.

The surge showed what it takes, and that it fits the settled decisions:
Direct2D already has the GPU effects (Gaussian blur, shadow, composite),
the `ID2D1HwndRenderTarget` every window uses casts to
`ID2D1DeviceContext`, and no dependency is added.

## What stays

- **Skeuomorphism.** Keys stay raised, the plate engraved, the lamps
  glass. A flat Folio look for the tiles was tried and rejected on
  2026-10-31 (commit `c6367bb`, reverted). This part changes how light
  and shadow are made, not the design.
- **Runes, not glows.** Activity is still shown by runes and small
  lights that move. Bloom makes a small emitter look lit; it is never a
  licence for big washes. The rule of thumb: if it would read without
  the bloom, the bloom may make it shine; if only the bloom shows it, it
  is a glow and is out.
- **Animation as a function of time.** The rain, the cube and the
  Folio's embers keep no state between frames. Particles here follow
  the same rule wherever they can.

## The technique

Three tools, used everywhere, replacing every fake.

1. **Bloom for light.** What gives off light is drawn sharp, on black,
   into a light layer of its own. That layer is then added over the
   window (`D2D1_COMPOSITE_MODE_PLUS`) three times: as it is, through a
   tight Gaussian blur and through a wide one. Light is only ever added,
   never painted over, so two lights meeting are brighter, and a hot
   core goes white. The emitter decides where light is; the bloom
   decides how it spreads.
2. **The shadow effect for shadow.** A key's shadow is its shape run
   through `CLSID_D2D1Shadow` (a real blur of its alpha), offset and
   tinted. It replaces `cast` and its eight shapes. Bevels and the
   hollow shade inside latched keys (`inner`, `hollow`) are the same
   effect inside a clip.
3. **Particles for anything that flies.** Sparks, runes round a lamp, a
   loot beam's motes, a task flying to a lamp, a lamp going out. Each is
   a small set of points with a birth, a speed, a life and a trail,
   drawn sharp into the light layer so the bloom makes them glow. The
   motion is pure and tested; the window only draws what it says.

## Architecture

### One light module for every window

`render/bloom.rs` moves up to `crate::light` and serves all three
drawing stacks: the cluster's `Painter` (`render.rs`), the panes'
(`glyphs.rs`) and the Folio's `Gfx` (`folio/view.rs`). Each window's
target keeps a `Light`:

- `begin(size)` hands back the light layer to draw emitters into, made
  once per size and cleared each frame.
- `present(dc, look)` blurs and adds it over the target.
- Nothing is made or blurred in a frame with no emitter, so a quiet
  window costs nothing. The surge already works this way.

`look` is a `Bloom { near, far, far_passes, gain }`, per window and per
theme (below), so the stage's glass can bloom softer than a tile.

### High dynamic range

The light layer becomes 16 bit float (`DXGI_FORMAT_R16G16B16A16_FLOAT`)
instead of 8 bit. In 8 bits two lights over 1.0 clip, and the bloom of
a clipped core is flat. In float they sum past 1.0 and the bloom
carries the extra out, which is what makes a lamp core read white hot
with colour round it. A tone curve (a `ColorMatrix` or the
`D2D1_TONEMAP` effect where present) brings it back into range before
it is added. Falls back to 8 bit where the device refuses float.

### Blur only what is lit

The bloom blurs the whole window today. Each emitter reports its
bounds; the light layer's dirty rectangle is their union, grown by
three times the wide sigma, and only that is blurred and added
(`imageRectangle` on `DrawImage`). A window with one working tile blurs
a strip 40 DIPs wide, not 300 by 900.

### Light that holds still

A lamp lit at rest (a finished turn's green, a waiting amber) does not
move, so its bloom belongs in the kept still layer, drawn once when the
still layer is. The still layer is a compatible render target, which
casts to a device context the same way. So:

- Still light: drawn into the still layer with the bloom baked in, at
  no per frame cost.
- Moving light: the per frame light layer, as the surge is.

### Particles

A pure module, `particles.rs` in the pure half of `horadric-ui`
(beside `motion.rs` and `folio_look.rs`), with tests:

- A `Burst` is a seed, a birth time, a count and a `Kind` (spark, mote,
  ember, ash, streak). A particle's place, size, heat and life at time
  `t` are a function of the seed and `t` alone, as `folio_look::ember`
  is, so nothing is stepped or stored per particle.
- Trails are the same particle sampled at a few earlier times, each
  fainter. In the light layer the bloom joins them into a streak.
- Heat, from 0 to over 1, maps to colour through one ramp: the phase
  colour when cool, through its hot mix, to white. Every emitter uses
  the same ramp, so all of Horadric's fire looks like one fire.

### Frame scheduling

Unchanged. The vsync clock (`vsync.rs`) beats while anything moves; a
covered or minimised cluster skips its frame (see
[performance](performance.md)). Particles only ask for frames while a
burst is alive.

### When there is no GPU

On a remote desktop or a software device, Direct2D falls back to WARP
and a blur is CPU work. `Light` checks the device once. Without a
hardware device it draws the emitters sharp with no bloom, which still
reads, and the shadow effect falls back to today's stacked `cast`.

## Inventory and migration

Counted on 2026-10-31: 37 fake light or soft edge calls in `render.rs`,
5 in `render/stone.rs` and `render/questlog.rs`, 3 in `folio/view.rs`,
the pane's own set in `glyphs.rs`, and 20 in the macOS port. Each step
below ends with a dev build the human looks at, and is committed only
when they like it. Steps 1 to 4 are the cluster window, since that is
where the human looks most.

### Step 1: the light module

- Move `render/bloom.rs` to `crate::light`, with `begin`, `present` and
  the `Bloom` look.
- Float light layer with a tone curve, and the 8 bit fallback.
- Dirty rectangle from emitter bounds.
- The software fallback.
- Measure: forty tiles, ten of them working, on a release build. The
  UI thread's share of a core with the bloom must stay within one point
  of without it. Write the numbers into the lore.

### Step 2: the cluster's moving light

All in `render.rs`, drawn per frame in `light()` today:

| Today | Shows | Becomes |
|---|---|---|
| `halo`, breathing | a waiting tile needs you | the key's rim as an emitter, amber, breathing; bloom spills it onto the plate |
| beckon (`halo` + `lamp`, `motion::beacon`) | a parked tile calls | the same rim, flashing |
| arrival ring | the moment a tile starts waiting | a ring of light leaving the rim, bloomed |
| `sparks` | subagents at work, no band | spark particles orbiting the lamp with trails |
| `hirelings` | each subagent's rune | the rune's strokes as emitters, carving with a hot cutting point; settling into the lamp as a burst of embers |
| `shimmer` | a context meter nearly full | a hot glint running along the meter's lit part |
| `power_down` | a session ended | the lamp's light collapsing to its middle and leaving as a few embers, no picture tube |
| `beams` | a finished turn, loot dropping | a shaft of light up off the key with motes rising in it |
| `flights` | a task flying to the lamp that took it | a streak particle with a trail, flaring as it lands |
| `astir` | Warriv at work on the quests tile | the tile's rim in gold, breathing, as an emitter |
| `busy_wash` | a project busy | stays a wash: it is a tint, not a light |

### Step 3: the cluster's still light and shadow

Drawn into the kept layer today:

- `lamp` at rest: its four spill rings and hot core become the lit
  glass as an emitter, bloomed into the still layer.
- `key` and `cast`: the eight step shadow becomes the shadow effect.
  The ledge (the key's side) stays: it is geometry, not light.
- `inner`, `hollow`, `latched`, `sunk`, `screen`: bevel and hollow
  shade through the shadow effect inside a clip.
- `led`: the header's count lights, a dot and a glow, become a dot as
  an emitter.
- The context `tank` and the trace stay as they are: they are marks,
  not light.

### Step 4: the other parts of the cluster window

- Runetome stones (`render/stone.rs`): the gold glow behind a stone and
  the rune marks become emitters.
- The quest log's glow dots (`render/questlog.rs`): emitters.
- Delete `glow_dot`, the ring loops, `BLUR_STEPS` and the theme tokens
  nothing uses any more. A grep for `glow_dot` and `BLUR_STEPS` finds
  nothing.

### Step 5: the stage

`glyphs.rs`, the panes and their bezels, and `sparks.rs`, the light
inside a terminal's glass:

- `tab_lamp`: the stacked dots become an emitter.
- `orbit`: the runes cut round the rim, and the gold flare at every
  tool: emitters, with the flare as a burst.
- `gathering` and `ash`: a typed letter settling white hot, a deleted
  one crumbling: the hot slices are emitters with the heat ramp; ash is
  ember particles.
- `sweep` and `rim`: a turn ending sweeps the glass and seals the ring:
  emitters.
- `raised_key`, `latched_key`, `sink`, `bezel`, `glass`: the shadow
  effect, as the keys in step 3.
- `summon` (the cube) and `rain` (the Matrix theme): their brightest
  cells as emitters, so the cube and the rain's heads glow. Light on
  text is the risk here; see open questions.
- The bloom for panes is softer than for tiles, since it sits beside
  text being read.

### Step 6: the Folio and the Arcane Sanctuary

- The Folio's embers (`folio/backdrop.rs`) are drawn without glow on
  purpose, to stay quiet behind reading. They get a faint bloom in the
  margins only, scaled by `folio_look::calm`, so they stay quiet under
  the column. The working dots get a small bloom.
- The composer's faceplate shadow: the shadow effect.
- The Arcane Sanctuary (`sky.rs`, `mind.rs`, `glyphs.rs` `sky`): nodes
  touched lately and a recall's sparks along the synapses are what this
  technique was made for: emitters, with the spark a streak particle.

### Step 7: the small windows

Toasts, menus, dialogs, Settings, the usage and Context windows, the
caption: their keys and sunk fields take the shadow effect; any lit
mark takes the bloom. Most have no light at all, so this is mostly
shadows.

### Step 8: macOS

`mac/paint.rs` fakes the same things in Core Graphics (`glow_dot`,
`cast`, `inner`, `hollow`), and `mac/look.rs` copies the tile's light
call by call. The Mac equivalent of the light layer is a Core Image
pipeline: emitters into a `CIImage`, `CIGaussianBlur` at the two
sigmas, added with `CIAdditionCompositing`, and `CIShadow` style
blurs for shadows; or a `CALayer` with `compositingFilter` set to
addition. Its own step in [macOS](macos.md), after the Windows look is
settled, so the Mac copies a finished design.

## Themes

Each theme gets its own `Bloom` look:

- Skeuomorph (the default): as tuned in step 1.
- Matrix: stronger and greener, a phosphor glow, the rain's heads lit.
- Winamp: tight bloom, almost none wide, for its hard LCD look.

## Settings

- The system's "show animations" off (`backdrop`, already read) turns
  off the moving light, not the still light: a lamp still glows, nothing
  flies.
- No setting of its own unless the human asks for one.

## Open questions

- **Bloom near text.** Light added over a pane's text could blur what
  is read. Proposed: the pane's light layer is masked off the text rows
  except where the light is meant to sit on a letter (a typed letter
  gathering). Try it on step 5 before committing to it.
- **DWM's rounded corners** cut the bloom at a window's edge. A lit rim
  near a cluster's corner may need its light kept inside a few DIPs.
- **Dark versus light themes.** Additive light on a light background
  washes out. Every theme today is dark; a light one would need the
  bloom off or inverted.
- **Recording and screenshots.** Float layers and effects should
  capture fine through DWM, but check `CopyFromScreen` once.
