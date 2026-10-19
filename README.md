# ansiopt

`ansiopt` rewrites a stream of text and ANSI escape sequences into a shorter
stream that leaves the terminal in the same state. It handles SGR, cursor
movement, modes, OSC (titles, hyperlinks, anything else), DCS/APC/PM/SOS
strings, character sets and plain ESC sequences. Anything it doesn't model
passes through byte for byte.

```
ansiopt [OPTIONS] [FILE]          # reads stdin when FILE is missing

  -o, --output <FILE>   write to FILE instead of stdout
  -r, --assume-reset    the terminal starts in its default state
      --visual          keep only what is visible (see below)
      --rep             use REP (CSI n b) for runs of one character
  -a, --aggressive      --visual --rep
  -c, --compat          no empty parameters, keep ST terminators
      --max-params <N>  parameters per written sequence (default 16)
      --stream          write output as input arrives
  -s, --stats           print sizes to stderr
      --show            print the result with escapes made visible

views (printed instead of the result unless -o is given):
      --diff            what changed between each piece of text
      --explain         what happened to every input sequence, and why
      --hotspot         the text shaded by escape bytes spent on it
      --all             in --diff and --explain, show everything
      --html <FILE>     an HTML report combining all of them
      --color <WHEN>    auto, always or never
      --width <N>       columns for the HTML preview
      --height <N>      rows for the HTML preview
```

As a library:

```rust
let out = ansiopt::minify(b"\x1b[0m\x1b[1m\x1b[31mhi\x1b[0m\x1b[0m", &Default::default());
assert_eq!(out, b"\x1b[;1;31mhi\x1b[m");
```

The HTML report's rendered preview uses the `vt100` crate through the
default `render` feature. Library users who only need minification can turn
it off with `default-features = false`.

`Minifier` is the streaming form. Call `feed` for each chunk, `flush` when the
producer may pause, and `finish` at the end. Two things only `minify` does.
It falls back to the input if the output would be longer. And visual mode's
self-disable (see below) covers the whole input instead of starting at the
chunk that triggered it.

## Results

Bytes saved on captured output from real tools:

| sample          |   bytes | default | `-r -a` |
|-----------------|--------:|--------:|--------:|
| `bat`           | 340,095 |   35.9% |   53.2% |
| `delta`         |  35,027 |    6.8% |   35.6% |
| `git log -p`    | 126,235 |    9.3% |   23.8% |
| `lsd -la`       |  63,006 |   24.9% |   35.4% |
| `nu` table      |  12,038 |   28.9% |   41.8% |
| `nvim` screen   |   5,217 |    4.4% |   39.6% |
| `eza -la`       |  30,867 |    7.9% |   18.8% |
| `rg`            |  13,776 |   12.7% |   12.7% |
| `cargo check`   |  28,159 |    6.2% |    6.2% |
| `git log --graph` | 25,563 |  0.9% |    0.9% |

Output that already writes one SGR per color change (git's graph, cargo) has
little to remove. Syntax highlighters and TUIs repeat themselves a lot.

## Seeing what changed

The views line the input up with the output on their text. The optimizer
never changes or reorders printed characters, only the escape sequences
between them, so each stretch between two characters can be compared on
its own. A byte-level diff would mostly show noise from sequences that were
merged or moved.

`--diff` lists every place where the sequences between two pieces of text
changed, with each sequence decoded. Changes that only spell the same thing
shorter (`\e[0m` becoming `\e[m`) are counted but hidden unless you pass
`--all`:

```
$ ansiopt --diff tests/fixtures/rg.ansi
ansiopt diff: 1,015 → 872 bytes (14.1% saved) · 48 of 49 gaps changed
(25 changes that only spell the same thing shorter, like \e[0m → \e[m, are not shown; --all shows them)

@@ in 24 · out 20 @@ …:    pub ▸fn …
- \e[0m \e[1m \e[31m   reset ; bold ; fg red
+ \e[1;31m             bold · fg red
```

It ends with the most common changes, with numbers generalized so
`38;5;196` and `38;5;197` group together.

`--explain` records what the optimizer did with every input sequence:
folded into a later SGR, merged with neighboring movements, dropped as
pointless, rewritten shorter, or kept as a barrier, with the reason.

```
$ ansiopt --explain tests/fixtures/rg.ansi
  offset  sequence                    meaning                         what happened
       0  \e[0m                       reset                           folded into \e[;32m at 0
       4  \e[32m                      fg green                        folded into \e[;32m at 0
```

`--hotspot` prints the text with each run shaded by the escape bytes spent
on it, and per-line totals for input and output. It shows at a glance where
a program spends its bytes.

`--html report.html` writes one self-contained page with tabs for all of
the above plus:

- a rendered preview of input and output side by side, drawn by `vt100`,
  with a check that every cell matches. Full-screen programs that leave the
  alternate screen show their last frame. vt100 is good for checking
  equivalence, not for screenshots: it has no DEC line drawing,
  strikethrough or hyperlinks;
- a breakdown of bytes per kind of content, before and after;
- hover details on every sequence: what it does and what happened to it.

Everything from the stream is HTML-escaped, and links in it are never made
clickable, so a report built from an untrusted capture is safe to open.

## How it works

### Tokens with their bytes

A small lexer follows the DEC/ECMA-48 state machine that xterm, VTE, kitty
and alacritty implement. Unlike `vte` it keeps the raw bytes of every token,
so malformed or unknown sequences can be copied out unchanged. I didn't use
`vte` for this reason. Its callbacks don't expose the original bytes, and
re-encoding what it parsed would change things the optimizer doesn't
understand. Input is treated as UTF-8. Stray C1 bytes and C1 code points
become barriers.

### Lazy state with unknowns

The optimizer tracks the pen (SGR attributes), the OSC 8 hyperlink and the
G0-G3 character sets twice. `desired` is what the input has asked for so far.
`emitted` is what the output has told the terminal. SGR, OSC 8 and charset
sequences only update `desired`. Nothing is written until something makes
those attributes visible:

- printed text needs everything;
- erase, insert/delete, scrolling and line feed need what erased cells take on;
- DECSC, unknown sequences and the end of the stream need everything.

At that point it writes the shortest SGR that turns `emitted` into `desired`.
It compares setting the changed attributes one by one (`22;2`) against a reset
followed by every non-default attribute (`;2`, with an empty parameter for the
reset), and splits long sequences.

Every attribute is tri-state, with unknown as the third value. The stream
starts unknown unless you pass `-r`, so `ESC[1m` printed into a terminal that
was already red stays red. A reset is only used when nothing is unknown,
because a reset would destroy attributes the input never touched.
Save/restore, DECSTR and similar operations keep only the values they are
certain about.

### Other rewrites

- Cursor movement: CUP makes earlier movements dead, same-direction moves add
  up, absolute moves absorb relative ones, and default parameters are dropped
  (`ESC[1;1H` becomes `ESC[H`, `ESC[1;5H` becomes `ESC[;5H`).
- Mode changes for independent flags (`?25`, `?2004`, `?1`, ...) are batched
  into one sequence and dropped when the terminal already has that value.
- Window titles that equal the current title are dropped.
- OSC strings end in BEL instead of `ESC \`, except queries, because the
  terminal answers with the terminator the query used.
- Redundant charset designations go away (`tput sgr0` writes `ESC(B` every
  time).
- `--rep` turns runs like `────────` into one character plus `ESC[7b`.

### Visual mode

`--visual` keeps what you can see instead of the exact cell contents. A space
or an erased cell shows background, underline, strikethrough and so on, but
not foreground, bold or italic. So those aren't refreshed for blank cells.
With `-r`, `ESC[31mfoo ESC[0m ESC[31mbarESC[0m` becomes `ESC[31mfoo  barESC[m`.
Without `-r` the reset stays, because it also sets a background that was
unknown before.
Reverse video turns this off, since it swaps foreground into the background.
A combining mark printed onto an earlier blank cell would expose the
difference. After cursor movement, or at the right margin with autowrap off,
that cell isn't even the one just printed. So visual mode switches itself off
when any character that might combine appears. Output printed *after* the
stream is out of its reach: a combining mark drawn onto one of those blanks
later shows the stale foreground. That's the trade-off of this mode.

## What is never assumed

Terminals disagree more than you'd expect. Every rule below came from a
mismatch the fuzzer found against vt100, alacritty (an earlier oracle) or
Ghostty:

- Colon sub-parameters (`4:3`, `38:2::r:g:b`) are barriers. Some terminals
  ignore them, others read `:` as `;` and turn `4:3` into underline plus
  italic.
- An SGR ending in an empty parameter (`ESC[31;m`) is a barrier. xterm reads
  it as a reset, Ghostty ignores it. Leading and middle empty parameters read
  as `0` in every terminal tested, which is why `ESC[;31m` is a safe reset.
- `21`, `6`, `20`, `58`/`59` and `60`-`65` are barriers. `21` means double
  underline or "bold off" depending on the terminal, and without underline
  color support `58;5;0` reads as "blink, reset". Dropping an earlier `4`
  because a later `21` replaces it would be wrong on a terminal that ignores
  `21`.
- Bold and faint replace each other in some terminals and combine in others,
  so when both are on the optimizer keeps their order.
- `38;5;1` and `31` are kept distinct. Bold-as-bright treats them
  differently.
- Tabs aren't plain movements. Alacritty records them in the cells they skip
  and wraps a pending line.
- With origin mode on, alacritty applies the top margin twice for CHA,
  Ghostty does the same for HPR, and vt100 ignores origin mode for VPA. CR,
  CHA, HPR and VPA rewrites therefore need origin mode known off.
- Some terminals only clamp relative movement at a margin when the cursor
  started inside the margins. Moves only add up when there are no margins.
- `0` is only dropped where every terminal reads it as the default. Ghostty
  treats `ESC[0L` and `ESC[0M` as no-ops, xterm as one line, so only `1` is
  dropped for IL, DL, ICH, DCH, ECH, SU, SD and REP.
- A hyperlink opened without `id=` gets a new identity from every OSC 8, so
  re-opening the same URL isn't redundant.
- Only `OSC 8;;` closes a hyperlink. Ghostty ignores an empty URI that comes
  with parameters (`OSC 8;id=1;`).
- OSCs can have side effects. Ghostty starts a fresh line on `OSC 133;A`, so
  any OSC not known to be quiet (titles, colors, cwd, clipboard,
  notifications) gets the full pen synced first.
- Ghostty keeps cursor state, hyperlink included, per screen, so every screen
  switch (`?47`, `?1047`, `?1048`, `?1049`) makes the pen unknown.
- An OSC cut short by another escape sequence must still be followed by
  `ESC`. The token after it is copied verbatim.

Most of the geometry rules start unknown and switch on once the stream sets
the mode, or right away with `-r`.

## Testing

The differential suite renders `prefix + input + suffix` and
`prefix + output + suffix` in independent emulators and compares every cell,
the cursor, the pen, modes and the title. Random prefixes put the terminal in
an arbitrary state first. Random suffixes check the state the stream leaves
behind. Failures shrink to a minimal case automatically. The harness is
`tests/harness/mod.rs` and runs against two oracles:

- the `vt100` crate, in `tests/oracle.rs`, so plain `cargo test` needs nothing
  but Rust;
- Ghostty's terminal core through [libghostty-vt], next to vt100, in the
  `ghostty-oracle` workspace crate.

The two disagree with each other on bold and faint, VPA in origin mode, what
erased cells keep, trailing empty parameters and more. That disagreement is
what makes the pair useful.

```
cargo test --release                                      # vt100, 20k cases per suite
cargo test --release -p ghostty-oracle                    # vt100 + Ghostty
ANSIOPT_FUZZ_ITERATIONS=200000 ANSIOPT_FUZZ_SEED=3 cargo test --release -p ghostty-oracle
ANSIOPT_CORPUS=dir cargo test --release -p ghostty-oracle corpus
```

The `corpus` test runs the captures in `tests/fixtures` (bat, rg, a nu
table with hyperlinks, vim, nvim) through both oracles, or the directory in
`ANSIOPT_CORPUS` instead. The fixtures were made from this repository's own
files; the nu table's `file://` links point at `/home/user/ansiopt` rather
than the machine they were captured on.

`tests/report.rs` checks the views. On random streams, the alignment's
pieces must rebuild both input and output exactly, tracing must not change
the output, and every token must end up with exactly one fate. Every fixture
goes through every view, and the HTML preview must report the two screens
identical.

`ghostty-oracle` builds Ghostty from source with Zig 0.16, which has to be on
`PATH`. It depends on libghostty-rs `main` at a pinned revision because the
crates.io release still needs Zig 0.15.2. On macOS 27 (Xcode 27's `ld`),
Ghostty's build currently fails while linking its shared library, with a
duplicate `_memset` between Ghostty's fast `memset` and Zig's compiler-rt.
Only the static library is needed. As a local workaround until it's fixed
upstream, point `GHOSTTY_SOURCE_DIR` at a checkout of the pinned Ghostty
commit with the shared-library install disabled. The real fix belongs in
Ghostty: `quirks_memset.zig` exports a strong `memset` for shared libraries,
and `initLibApple` links the dylib from the static archive with `-all_load`,
which also pulls in compiler-rt's copy.

```
git clone https://github.com/ghostty-org/ghostty ~/src/ghostty-vt
git -C ~/src/ghostty-vt checkout 3425025e585a3403340d4dc6d65132f42e05e605
# in build.zig, change `shared.install(b.getInstallStep());` to
#   if (!config.target.result.os.tag.isDarwin()) shared.install(b.getInstallStep());
GHOSTTY_SOURCE_DIR=~/src/ghostty-vt cargo test --release -p ghostty-oracle
```

The fuzzer can't see kitty extensions, sixel, or attributes neither emulator
implements (fonts, superscript, framed, proportional spacing). The optimizer
still treats those consistently. Each one is either fully modeled or passed
through as a barrier.

[libghostty-vt]: https://github.com/uzaaft/libghostty-rs
