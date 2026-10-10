# Plug's brand

Plug is one character: a plug with a face. It is the app icon, the menu bar
icon, and the picture next to everything Plug says about itself. This page is
how to draw it, colour it, and move it.

<p align="center"><img src="assets/plug-icon-animated.svg" width="120" alt=""></p>

## The character

Two prongs, a rounded body, two tall eyes. It leans 10 degrees to the left
and looks straight ahead. Nothing else: no mouth, no arms, no cord. This is
the character at rest; its other faces are under [Faces](#faces).

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
white. Inside the app the character has no tile and takes one flat colour.
In the menu bar panel it is Plug blue, and its face says how Plug is. In the
window's banner it takes the colour of what it is saying. In the menu bar it
takes the menu bar's own colour, like every other menu bar icon.

### Status colours

Four colours say how something is, and each means one thing everywhere: a
server's dot, the banner, the wash behind a card, the mark beside the
character. In the app they are named once, in `StatusColor`.

| Colour | Means |
|---|---|
| Green | Working |
| Grey | Starting, or off |
| Orange | Needs you: sign in, allow, reload |
| Red | Stopped or failed |

Blue is not a status. It is Plug itself, and the one button to press.

Rules:

- Colour is never the only signal. Beside every coloured dot or wash there
  are words, a shape, or a face that say the same thing.
- One blue button on a screen at most: the single thing to press. When
  several things each have a fix, none is blue.
- A card that reports trouble takes a light wash of the worst status in it,
  orange or red. A card that only invites, such as adding a first server,
  takes none.
- The character in the panel stays Plug blue whatever happens. Only the
  small mark beside it takes a status colour, orange or red. The "z"s when
  Plug is off are grey.

## Layout in the panel

- Things are set apart by space, not lines. The panel has one divider, above
  its controls. A card may have one hairline, above its last line.
- Every gap is a step of the app's one spacing scale (`Metric`).
- A fix sits at the end of the row it fixes, never in a list of its own.
- A plain line that says something and offers a button starts with one
  small grey line icon, drawn for Plug in one weight (`PanelIcon`), that fits
  both the words and the button: a stethoscope beside Run Checkup, a plug
  with a loose cord beside Start Plug, a server's tile with a plus beside
  Add Server. No stock symbols there.

## Faces

The character has a face for each thing Plug can be. It has no mouth, so a
face is made of four things: how long each prong is and how far the two lean
apart, how big each eye is, a lid that comes down over an eye from above, and
a cheek that pushes up into it from below. The prongs work like ears: up when
it is pleased, down and apart when it is worried, one up and one down when it
is not sure.

| Face | When | What it looks like |
|---|---|---|
| Awake | Everything is working | The icon. It blinks, and now and then glances to one side. |
| Happy | Something just came right | Cheeks up under the eyes, prongs up, a small bounce. Then back to Awake. |
| Cheering | A first, or a first-run step done | Happy, with a hop, and five sparks fly off its prongs |
| Wink | You clicked it | One eye shut, a small hop |
| Curious | A page with nothing on it yet, or no servers yet | Tilts its head the other way, one prong up and one down, then back |
| Surprised | Only when you play with it | Wide eyes, prongs straight up |
| Working | Starting or busy | The prongs take turns going up, and the eyes follow |
| Thinking | A page has been loading for a while | Leans over and looks up, lids half down, one prong up |
| Loading | A page is loading | The two prongs and the body become three dots that hop in turn |
| Needs you | Sign in, or allow something | Eyes round, prongs up, and a dot beside it |
| Not sure | Only when you play with it | One eye narrow, one prong down, and a "?" beside it |
| Dizzy | Several servers need something at once | Its eyes are two swirls that turn together, it sways from its feet, and a "?" sits beside it |
| Worried | A server stopped, or Plug needs repair | Shakes its head once. Lids slant in, prongs down and apart, and a dot beside it. |
| Alert | Plug cannot finish setting up | The prongs and the body become an exclamation mark that hops now and then |
| Out | Plug itself is stopped | Keels over with crosses for eyes and a "!" beside it. Only this state gets the crosses. |
| Asleep | Plug is off | Leans further over, eyes shut, breathes slowly, and "z"s drift up |
| Tucked away | Only when you play with it | The three parts become one dot |

Click the character and it winks. Keep clicking and it goes through every
face it has, then goes back to the one for how Plug is.

## Motion

The character moves to say what state Plug is in. It never moves to decorate.

Going from one face to the next, it changes shape: the prongs grow or
shrink, the eyes change size, the three parts slide apart into dots and back.
Nothing is ever swapped for something else. Every part rides a spring, so it
arrives with a little overshoot and settles.

In the menu bar it blinks every 6 to 14 seconds while Plug is on, and looks
left and right while Plug is busy. Off, it is an outline with its eyes shut.
The menu bar icon keeps the icon's shape; the faces are for inside the app.

Rules:

- Motion follows state. If the state has not changed, nothing new happens,
  beyond a blink, a glance, or a breath.
- Shape changes, never swaps. A new face is the same three parts and two
  eyes, moved.
- One colour at a time. The character never changes colour mid-movement.
- Sparks are for firsts. Nothing that happens every day earns them.
- Nothing loops faster than once a second, except the three dots while a
  page loads.
- With Reduce Motion on, the character holds still in the face for its state.

## Where it goes

| Place | What is there |
|---|---|
| App icon, Dock, notifications, updates | The tile |
| Menu bar | The character alone, with a small badge when something is happening or wrong |
| Menu bar panel | The character in Plug blue, at the start of the top row, beside the one line Plug says about itself and the switch. Under it: a card for whatever needs you, the servers that are fine as a shelf of icons, who is connected, and the last tool call. |
| Window banner | The character, in the colour of what it says, beside the one sentence Plug says about itself |
| A page Plug cannot fill | The character, large, above the reason |
| A page that is loading | The character as three dots, then thinking if it takes a while |
| A page with nothing on it yet | The character, tilting its head, above what would be here and the button that adds it |
| The foot of the window, at a first | The character, sparking, beside what just became true |
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
| `PlugApp/PlugApp/Views/PlugCharacter.swift` | The character in the app: its shapes, its faces and its motion |
| `plug-core/src/http/oauth_ui/plug.css` | The look of the sign-in pages |

After changing the source drawing, run `./scripts/render-icons.sh` to redraw
every size, and update the shapes in `PlugCharacter.swift` to match.
