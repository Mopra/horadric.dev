# GPU light: every glow, shadow and spark drawn the way a game draws them

Proposed 2026-10-31. Built so far: the working lamp's surge (the human
called it much better), the light module every window can keep
(`crate::light`), particles (`particles.rs`), the cluster's moving
light (`render/shine.rs`) and its still light and shadow
(`render/shade.rs`), and on 2026-11-08 steps 4 to 7: the rest of the
cluster, the stage, the Folio, the Sanctuary and the small windows. The
lore has how. What is left on Windows is the human's look and the
measure; then the Mac (step 8).

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

### Light that holds still

A lamp lit at rest (a finished turn's green, a waiting amber) does not
move, so its bloom belongs in the kept still layer, drawn once when the
still layer is. The still layer is a compatible render target, which
casts to a device context the same way. So:

- Still light: drawn into the still layer with the bloom baked in, at
  no per frame cost.
- Moving light: the per frame light layer, as the surge is.

### Frame scheduling

Unchanged. The vsync clock (`vsync.rs`) beats while anything moves; a
covered or minimised cluster skips its frame (the lore has the
performance passes). Particles only ask for frames while a
burst is alive (`Burst::alive`).

## Inventory and migration

Counted on 2026-10-31: 37 fake light or soft edge calls in `render.rs`,
5 in `render/stone.rs` and `render/questlog.rs`, 3 in `folio/view.rs`,
the pane's own set in `glyphs.rs`, and 20 in the macOS port. Each step
below ends with a dev build the human looks at. Steps 2 to 4 are the
cluster window, since that is where the human looks most.

### Waiting on the human

Steps 1 and 2 were approved by the human on 2026-11-06 from screenshots.
Step 3 is built (2026-11-06) and waits for the same look, from
screenshots of all three themes. Two checks are left from step 1:

- Measure: forty tiles, ten of them working, on a release build. The
  UI thread's share of a core with the bloom must stay within one point
  of without it. Measured on 2026-11-06 it did not hold in the first
  minute: about 9 % of a core with the bloom against 5 % without (the
  lore has every sample). Find what the bloom costs the UI thread there.
  Step 3 drew the wide glow once instead of once a pass; not measured
  since.
- Check a remote desktop session once: the software fallback should
  draw the emitters sharp. A screenshot (`CopyFromScreen`) captures the
  float layer.

### Step 3: the cluster's still light and shadow

Built 2026-11-06: still light bloomed into the kept layer and bloomed
again only when it changes, shadow and shade through the shadow effect
baked once per shape; the lore has how.

### Steps 4 to 7: built 2026-11-08, waiting on the human's look

Built in one go; nothing was looked at on screen yet. What to look at:
a running Runetome stone (gold rim and lit runes), the quest log's
working and awake dots, the Settings portal's specks, a toast's lamp; a
working pane's ring (hot runes, a spark burst at each tool), typing and
deleting in a pane, a turn ending (the sweep, now a line without its
stepped tail), a waiting pane's rim (one line, the bloom its glow), a
browser tab's lamp, the Matrix rain's heads, the cube; the Folio's
margins while Claude works and its composer's shadow; the Sanctuary's
lit nodes and a recall's sparks. Bloom near text was not masked: the
pane's bloom is the softer `Bloom::pane()` and only what is meant to
sit on a letter (a typed one) lights the grid. If the ring's or the
rim's glow blurs the edge columns, mask then.

Cost to watch: a Matrix pane blooms its rain heads on every rain frame,
and a working Folio blooms its margins 25 times a second. Both blur
only what is lit, but the lit area is most of the window.

### Step 4: the other parts of the cluster window

- Runetome stones (`render/stone.rs`): the gold glow behind a stone and
  the rune marks become emitters.
- The quest log's glow dots (`render/questlog.rs`): emitters.
- Delete `glow_dot`, the ring loops, `BLUR_STEPS` and the theme tokens
  nothing uses any more. A grep for `glow_dot` and `BLUR_STEPS` finds
  nothing outside `mac/`, which keeps its own until step 8.

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
  flies. Checked 2026-11-08 window by window; the rain, the Folio's
  embers and the Sanctuary's sparks did not stop and now do.
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
