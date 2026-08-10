# Changelog

## Website makeover

A full redesign of the web app, a restructure of the code behind it, and a
test suite for the transfer logic.

### Design

- **New landing page.** Single page: a full-viewport hero, "How it works",
  and an FAQ. Everything below the hero sits past the fold, with a chevron as
  the cue to scroll. Replaces the old home page and the separate `/about`.
- **New send and receive screens.** No boxes — the instruction, the code and
  the progress each own their page. A `01 / 02 / 03` step indicator on both
  sides shows where you are.
  - **Send:** the whole viewport is the drop target, with the state shown in
    the headline rather than an overlay. Once files are chosen they list on
    hairlines with per-file sizes and a running total.
  - **Share code:** the code is the headline; the link and copy button sit
    beneath it and the QR folds away behind a toggle.
  - **Receive:** the share code is eight cells. Pasting a full share link
    works — the code is extracted from it.
  - **Offer:** file rows on hairlines with the total, and weighted actions
    (accept wide and solid, reject quiet).
  - **Transfer:** the percentage is the headline, with rate, ETA and byte
    counts in one row and a hairline bar per file.
- **Palette and type.** Semantic colour tokens (`canvas`, `surface`, `ink`,
  `flame`, `azure`, …) that flip between light and dark, replacing scattered
  `dark:` variants. Source Sans 3 and JetBrains Mono, self-hosted.
- **Header** reduced to the logo, GitHub and the theme toggle — no bar, no
  border.
- **Brand assets.** Logo as SVG and PNG, favicon, app icons and an OG image,
  all generated from one definition of the mark.

### Behaviour

- **Drag and drop works across the whole window**, so the header and footer
  are no longer dead zones. Only drags carrying files trigger it; text
  dragged into an input still behaves normally.
- **Disconnects are noticed in real time.** A peer that closes its tab used
  to leave the sender streaming into nothing, because only the data channel's
  `close` event was watched and an abrupt departure never sends one. The
  connection state is now watched too.
- **Leaving mid-transfer asks first**, via an in-app dialog rather than
  `window.confirm()`, which blocks the main thread and stalls the transfer
  while it is open.
- **Resume, folders and the no-File-System-Access path** all still work; the
  receive screen and the offer now say plainly what each mode implies,
  including the memory a buffered transfer will need.
- Fonts are self-hosted, so the site makes **no third-party requests**.

### Code

- One receiver instead of two. The File System Access API decides only where
  bytes land (a `TransferSink`), not which pipeline runs — the two receivers
  were 67% identical.
- Relay handshake, SDP exchange and the control channel live in one place
  (`transport/session.ts`, `transport/control.ts`) instead of three copies.
- Capability detection resolved once into a `disk | download` union, instead
  of fourteen scattered checks.
- Transfer state machines lifted out of the pages into primitives, leaving
  the pages as markup.
- One `TransferEvent` union in place of two bags of callbacks.

### Tests

32 tests. Complete transfers on both storage paths, interruption from either
side, resume, rejection, backpressure and disconnect detection — driving the
real shipped modules against an in-process fake browser, with no headless
browser and no network.

Plus a protocol-compatibility suite that reads the Rust definitions in
`fsend-cli` and `fsend-relay` and asserts the browser still speaks the same
wire format: version string, message tags, field names, fragment framing and
gzip. A rename on either side fails the build.

`bun run test`
