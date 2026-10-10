# Plug's brand

Plug is one character: a plug with a face. It is the app icon, the menu bar
icon, and the picture next to everything Plug says about itself. This page is
how to draw it, colour it, and move it.

<p align="center"><img src="assets/plug-icon-animated.svg" width="120" alt=""></p>

## The character

Two prongs, a rounded body, two tall eyes. It leans 10 degrees to the left
and looks straight ahead. Nothing else: no mouth, no arms, no cord.

The one drawing everything comes from is `docs/assets/plug-icon.svg`, on a
120 by 120 grid:

| Part | Shape |
|---|---|
| Prongs | Two rounded bars, 14 wide, 37 tall, fully round ends |
| Body | 72 wide, 54 tall, corner radius 18 |
| Eyes | Two rounded bars, 9.24 wide, 22.28 tall, 25 apart centre to centre |
| Lean | The whole character turned 10 degrees left around its middle |
| Tile | Rounded square, corner radius 27 |

Rules:

- Keep the lean. Upright, it is a socket, not a character.
- Keep the eyes level with the body. They turn with it.
- Do not add features, outlines, shadows on the character, or a second colour
  inside it.
- Leave clear space around it of at least one prong width.
- On the tile, the character's farthest point sits just inside the outer
  circle of Apple's icon grid. Do not make it bigger.

## Colour

| Name | Value | Use |
|---|---|---|
| Plug blue, top | `#3d9bff` | Top of the tile gradient |
| Plug blue | `#0a6ee6` | Bottom of the tile gradient, links, badges, lines in diagrams |
| Eye blue | `#0a5fd0` | The eyes, on the white character |
| White | `#ffffff` | The character, on the tile |

The tile is always the blue gradient, top to bottom, with the character in
white. Inside the app the character has no tile and takes one flat colour:
the colour of what it is saying.

| Plug is | Colour |
|---|---|
| Working well | Green |
| Starting, or off | Grey |
| Needs you | Orange |
| Stopped by something | Red |

In the menu bar it takes the menu bar's own colour, like every other menu bar
icon.

## Motion

The character moves to say what state Plug is in. It never moves to decorate.
One state, one movement, and the movement is small.

| Plug is | The character | Timing |
|---|---|---|
| Working well | Blinks. Now and then it glances to one side and back. | A blink every 2.5 to 6 seconds, about 0.2 seconds long. A glance after one blink in three. |
| Starting or busy | Looks left, then right, and keeps going. | 0.8 seconds each way |
| Wrong | Shakes its head once, then blinks. | Four quick turns, about half a second in all |
| Fixed | Hops once, then goes back to blinking. | Up in 0.14 seconds, a springy landing |
| Off | Leans further over, shuts its eyes, and breathes slowly. | 2.6 seconds in, 2.6 out |
| Loading a page | Looks left, then right, in place of a spinner. | 0.8 seconds each way |
| A first-run step done | Hops once. | As above |
| Clicked | Hops once. | As above |

In the menu bar it blinks every 6 to 14 seconds while Plug is on, and looks
left and right while Plug is busy. Off, it is an outline with its eyes shut.

Rules:

- Motion follows state. If the state has not changed, nothing new happens.
- Only the eyes, the lean, and a small hop or breath ever move. The shape
  never stretches, spins, or changes colour mid-movement.
- Nothing loops faster than once a second.
- With Reduce Motion on, the character holds still in the pose for its state.

## Where it goes

| Place | What is there |
|---|---|
| App icon, Dock, notifications, updates | The tile |
| Menu bar | The character alone, with a small badge when something is happening or wrong |
| Menu bar panel and window banner | The character, moving, beside the one sentence Plug says about itself |
| A page Plug cannot fill | The character, large, above the reason |
| A page that is loading | The character, looking from side to side |
| First-run guide | The character between Servers and Clients |
| Settings, About | The tile |
| README and docs | The tile; in the big picture it blinks |
| MCP server icon that clients show | The tile |
| Sign-in pages in the browser | The tile at the top and in the tab, over a white card with Plug blue buttons |

Where it does not go: next to a server, a client, a tool, or an event. Those
have their own icons. The character only ever stands for Plug itself.

## Words

The character is called Plug. The words Plug uses are in
[VISION.md](VISION.md): Server, Tool, Client, Event, Activity.

## Files

| File | What it is |
|---|---|
| `docs/assets/plug-icon.svg` | The source drawing. Change this one. |
| `docs/assets/plug-icon-animated.svg` | The same, blinking, for web pages |
| `docs/assets/plug-icon-*.png` | Sizes for docs and the MCP server icon |
| `docs/assets/social-preview.png` | The picture for links to the repository |
| `PlugApp/PlugApp/Assets.xcassets/AppIcon.appiconset/` | The Mac app icon |
| `PlugApp/PlugApp/Views/PlugCharacter.swift` | The character in the app: its shapes and its motion |
| `plug-core/src/http/oauth_ui/plug.css` | The look of the sign-in pages |

After changing the source drawing, run `./scripts/render-icons.sh` to redraw
every size, and update the shapes in `PlugCharacter.swift` to match.
