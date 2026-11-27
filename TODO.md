# Handover: next steps (2026-11-27)

This file hands over to the next session. It covers what was done on
`claude/gallant-hawking-su3jk1`, now merged into `personal-build`, what was
measured, and the three next steps in order.

## Where things stand

- **Hardware tested on:** RX 7900 XTX, Windows 11. Every runtime test runs on
  the owner's machine. The cloud sessions can only build for the
  `x86_64-pc-windows-msvc` target:
  ```bash
  CARGO_FEATURE_PURE=1 cargo check --locked --target x86_64-pc-windows-msvc --workspace
  ```
  `CARGO_FEATURE_PURE=1` makes blake3 skip its assembly, which needs `ml64.exe`.
  Baseline clippy (`-p victory-gpu --all-targets --features diagnostics`) has
  6 warnings, all inherited from `main`: texture.rs `size`, panel.rs,
  place.rs `from_upload`, rtstamps.rs ×2, and watch.rs:3626.
- **The usual test spot:** load the save, escape the prison, then walk to the
  coffee shop. That is about 6 minutes after loading. The glitches show up
  there and in the café cutscene.
- **How runs are made** (`-Exe` last, the owner's preference):
  ```powershell
  scripts\play.ps1 -WarmupOnly -Fast
  scripts\play.ps1 -Fast -Log run.log -Exe '<exe>'
  ```
  - `-Fast` gives the Series S profile plus renderer patches 1, 2, 5, 6 and 7.
  - `-Log` tees the runtime's stderr to a file.
  - `VICTORY_AB='60,<SWITCH>'` alternates a switch every 60 s. It only works
    for switches read through `crate::ab::pick`. **Port 2
    (`VICTORY_PORT_DESCRIPTORS`) and the builder cannot be A/B'd**: they act at
    load. Use separate runs for those.
  - To shrink a log:
    ```powershell
    Select-String -Path run.log -Pattern ' fps, |A/B phase|renderer port 2|FidelityFX|ERROR|removed' | Set-Content summary.txt
    ```
- **Upscaling:** with `VICTORY_FIDELITYFX_DIR` (or `[paths].fidelityfx_dir`)
  set, the PC's FSR 4.1.2 now serves the title's own FidelityFX calls on every
  profile (`apps/victory/src/run.rs`; `VICTORY_FSR_HOST=0` turns it off).
  - This fixed the Series S speckling and smearing. That came from the
    console's FSR 4: its precompiled pipelines fail to load here, so its 27
    passes ran through our translator.
  - `VICTORY_UPSCALER=6` (the title's own upscaler) is the fallback when no
    folder is set.

### What the batching work measured

Every new switch is A/B-able. All results are from the same spot, Series S
with the renderer patches unless noted.

| Switch | Default | Result |
|---|---|---|
| `VICTORY_READBACK_POOL` | on | within noise; idle in that scene (no water) |
| `VICTORY_UPLOAD_SLICES` | off | zero buffer used 1–5×/frame; slices never used under `VICTORY_CARD_STEPS=2`, which `play.ps1` sets. **Dead in practice** |
| `VICTORY_BETWEEN_POOL` | off | not measured |
| `VICTORY_JOIN_BARRIERS` (plus folding) | off | 52.05 vs 52.2 ms: noise; about 300 calls/frame saved, 80 barriers folded |
| `VICTORY_IA_CACHE` | off | noise; about 260 calls/frame skipped |
| `VICTORY_TABLE_GATHER` | off | not measured |
| Heap starts, constant-buffer slot pairs, cached `GetDesc` flags | always on | no switch; no regression seen |
| `VICTORY_D3D12_DRED=faults` instead of `light` | — | owner saw "at most 2 fps"; no logs yet |

Conclusion: the frame (about 52 ms on Series S, about 59 ms on Series X) is
spent "preparing", mainly copies (about 20 ms) and copying back (about 30 ms).
D3D12 call batching does not move it.

## Step 1: character mesh glitches (top priority)

### Update 2026-11-27, later session: compared with Belobug's `a-publier`

The owner ran Belobug/VIctory-Belobug-AMD `a-publier` (merge base with
`personal-build`: cd8dfc0) with `play.ps1 -RayTracing`: Series X profile, no
renderer patches, builder on. **No glitches**, at under half the frame rate.
That contradicts "the builder made it worse" above only on its face: that run
was on our build with `-Fast`.

Both builds then shared one shader cache. Each now has its own
(`[paths].cache_dir`). On a cold cache some faces are missing on the first run
only (draws skipped while shaders translate); back on the second. Not a bug.

A read-only audit of every difference between the two branches (8 agents,
each strong suspect checked by another trying to refute it) found **no code
difference that explains the glitch**:
- Shared cache: real hazard (both keyed `version 2`, first writer wins,
  `shader_cache.rs` `cached_by` serves the other build's entries as "stale"),
  but the only output difference, Belobug f962387 (`dx.op.getDimensions` kept
  out of REUSE, an RDNA4 compiler-crash workaround), merges only calls with the
  same SSA handle and is exact; skinning's raw vertex loads emit no
  getDimensions (`emit/lower.rs` emits it at 5 sites, none for buffer loads).
- Ours-only, default-on, absent from Belobug (c18ff12 placed-view padding,
  c404ee2, 39bf8f6, ee6757a, read-back pool, port RT bits, …): each refuted
  or rated low; none feeds the skinning landing path.
- Belobug 4ad8527 (port 2 sync parts disjoint) acts only with
  `VICTORY_PORT_DESCRIPTORS=1`.
- The skinning landing path (card steps stage 2, relay, card buffers:
  `host/card_steps.rs`, `shadow.rs`, `land.rs`, `host/send.rs`) is the same
  code on both branches.

**Hypothesis now:** a timing race in that shared landing path, hidden by the
slow configuration (Series X, no patches, builder: 200-500 ms a frame on ours).

Runs that decide it, after `play.ps1 -WarmupOnly <same switches>`:
1. Ours `-RayTracing -Log pb-rt-1.log` against Belobug's clean run (like for like).
2. Ours glitches, Belobug not: a real code difference after all. Bisect
   locally, one cache folder per build: build cd8dfc0 first; clean →
   `git bisect` cd8dfc0..personal-build; glitched → bisect Belobug's merges.
3. Both clean: race or configuration. On ours, plain `-Fast` (expected
   glitch), then `-Fast` with one of `VICTORY_CARD_STEPS='0'`,
   `VICTORY_RELAY='1'`, `VICTORY_RELAY='0'`, `VICTORY_CARD_BUFFERS='0'`
   (lead 1 below). The first that clears it names the subsystem.

### What -Fast costs in picture with the builder on

The intro's phone screen shows whenever the builder is on (`-RayTracing`, with
or without `-Fast`) and is black without it: a ray-tracing matter, not Step 2.
`-RayTracing` alone looks more accurate than `-RayTracing -Fast`. To find what
`-Fast` loses: `-RayTracing -Fast` with one `VICTORY_PORT_<X>='0'` per run
(CONSTANTS, DESCRIPTORS, CBV, GUARD, JOURNAL), and one `-RayTracing -Console
series-s` (the profile without the patches).

**Leads, in the order to test them:**
1. **The skinning passes are special to the layer.** The doc comment on
   `stage()` in `crates/victory-gpu/src/host/card_steps.rs` says eleven
   dispatches a frame write through one view of the title's whole 174 MB
   heap, at places read from job records, so no bound on their writes is
   known in advance. The relay, landing and card-steps machinery decides when
   those bytes reach the GPU's copy and title memory. A one-frame stale or
   partly landed skinning output gives exactly these glitches.
   - Owner runs, one change each:
     - `-Fast` against `-Safe -Fast`.
     - `-More @{VICTORY_CARD_STEPS='0'}`.
     - `-More @{VICTORY_RELAY='0'}`, if that is legal; check `relay_exact` and
       `stage()` first.
     - `VICTORY_CARD_BUFFERS=0`: copies in the PC's memory. Slow, but a clean
       test.
   - The first switch that removes the glitches names the subsystem.
2. **Upstream Marvisak/VIctory#1567.** `CS_PedStandardSkinning` variants
   `_4`, `_7` and `_8` read a bindless raw vertex buffer whose descriptor
   index comes from a constant buffer at offset 0x40. A null or stale host
   slot gives zeros or another character's data. Check how this fork resolves
   dynamically indexed raw views; see `descriptor/host.rs` and
   `draw.rs` `tables`/`fresh_tables`. Check whether the slot can be stale for
   a frame.
3. **Port 2's fallback** (only relevant when `-Fast` is on). About 290 view
   records a frame are missed and fixed late. `VICTORY_PORT_COPIES=1` closes
   the memcpy gap. That is not the cause of the base glitches, which happen
   without patches, but it can add to them.

**Done when:** the coffee-shop walk and the café cutscene show no
strap spikes or outfit swaps in a `-Fast` run, and the fix is a switch that
can be A/B'd, or is plainly correct.

## Step 2: shader writes to placed textures are thrown away

Every run logs about 12 times:
`placed unordered texture view not mirrored (null: writes through it are lost)`.
The code is `crates/victory-gpu/src/descriptor/host.rs`, around the
`shortcut:` comment near `placed_texture(view)`.
- The textures are 1024×1024 with formats 71 and 99 (DXGI BC1 and BC7 sRGB).
  That looks like the title encoding or streaming block-compressed textures
  on the GPU.
- Writes through those views go nowhere today.

Likely visible as: late or wrong textures, the README's "textures appear late
while the title streams". The black phone screen is ray tracing, not this (it
shows whenever the builder is on). The wrong eyes are still to be checked.

**Blocked on a log line:** D3D12 gives BC1/BC7 textures no unordered access,
so mirroring these means a texture of 64/128-bit unsigned blocks (a quarter of
the size each way) that shaders write, copied into the BC texture after the
dispatch (and back to title memory as the title expects). Whether that fits
depends on the view's own format and dimension, words `desc[0..10]` in the
warning; the owner's logs have them:
```powershell
Select-String -Path run.log -Pattern 'placed unordered texture view not mirrored' | Select-Object -First 12
```

To do:
- Find who writes those textures and how a placed texture's unordered view
  could be mirrored. Shader views of such textures already get a resource
  when a dispatch writes one; follow that path.
- Then compare texture pop-in at the test spot.

## Step 3: tidy up and settle defaults

- **Remove switches that measured as nothing,** or make them clearly
  experimental:
  - `VICTORY_UPLOAD_SLICES`: kept, experimental. Its slices only act with
    card steps below 2, which the Step 1 run `VICTORY_CARD_STEPS='0'` uses;
    its buffer of zeros acts either way;
  - `VICTORY_IA_CACHE`: **removed** (3125df6);
  - keep `VICTORY_JOIN_BARRIERS` (it is cheap and may matter once the card is
    the limit) but leave it off.

  Measure `VICTORY_BETWEEN_POOL` and `VICTORY_TABLE_GATHER` once, or drop them.
- **DRED:** get one pair of logs, plain `-Fast` against
  `-More @{VICTORY_D3D12_DRED='faults'}`. If `faults` saves 3 ms or more of
  "preparing", make it the `play.ps1` default and keep `light` for crash hunts.
- **README:** done: `Play-Watched.ps1` replaced by the `play.ps1` command;
  `-Fast`, `-Console`, `-Log`, `-WarmupOnly`, the pipeline book, the PC FSR on
  every profile, one cache folder per build and the A/B limits documented.
- **Warm-up:** done: `play.ps1 -WarmupOnly` (shaders, then the pipelines in the
  book), the pipeline book on by default, `victory warm` says which folder it
  read and when the book there is empty.
- **Upstream:** none of this work duplicates an upstream PR. The closest are
  Marvisak/VIctory#1382, #3129, #2215, #2217 and #2818, all open or closed
  without merging. If anything goes upstream, the PC FSR on every profile and
  the port 2 log fix are the most self-contained.
