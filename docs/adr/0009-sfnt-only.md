# 0009 — fontina reads the sfnt family, and says so when it will not read a file

**Status:** accepted, 2026-09-05.

## Context

fontina reads TrueType, OpenType, collections of either, and both WOFF wrappings. It
reads every outline format in them — `glyf`, CFF and CFF2 — all five colour formats
— COLRv0, COLRv1, `SVG `, `sbix` and `CBDT` — variable axes, named instances and
bitmap strikes. On a stock macOS installation that is 518 of 519 files, and the one
failure is a font with no `head` table, correctly reported as a parse error.

It does not read Type 1 in either wrapping, BDF, PCF, Windows `.fon`/`.fnt`,
resource-fork `.dfont`, or EOT.

That boundary has been true since M0 and was written down nowhere. Not in the manual,
not in the FAQ, not in a decision record. A person with a library of Type 1 fonts had
no way to learn from the project whether fontina could not read them, would not read
them, or had a bug.

Two facts make this worth deciding rather than leaving implicit:

- **Type 1 is not a historical curiosity to the people who own it.** Adobe ended
  support in January 2023. Anyone with a design library assembled before roughly 2005
  has a shelf of fonts that no current application will open, and they are exactly the
  people looking for a font manager that can tell them what they have.
- **macOS still ships two of them.** `HelveLTMM` and `TimesLTMM` are datafork Type 1
  Multiple Masters, with no file extension at all.

## Decision

**fontina reads the sfnt family and nothing else.**

Parsing goes through fontations (ADR 0001), which reads sfnt and has no interest in
becoming a general font library. `CLAUDE.md` forbids hand-parsing tables that
fontations already exposes, and hand-parsing tables it does not expose — a Type 1
charstring interpreter, a BDF lexer — is the same mistake wearing a different hat: it
would be the second parser in a program whose whole claim is that it has one.

The alternative is a second dependency per format, each one more code reading hostile
input in a program that has spent four milestones getting that surface small enough to
fuzz.

**But not reading a font is not a licence to be silent about it.** Since cairn 0018 a
scan recognises these formats by content and names them:

```
skipped 2 font(s) in a format this program does not read: Mac resource fork
  - /System/Library/Fonts/HelveLTMM (Mac resource fork (.dfont, datafork Type 1))
```

That is the half of this decision that matters. A tool that quietly omits files is
worse than one that refuses them loudly, because the person cannot tell the difference
between a font it cannot read and a font that is not there.

## Consequences

- The formats listed above will not be indexed, previewed, activated or exported.
- A scan says which files it walked past and what they are, so the omission is visible
  at the moment it happens rather than discovered later.
- Recognition is by content, not by extension, because the two fonts that prompted this
  have no extension.
- The FAQ answers the question directly, so somebody who searches rather than reads
  finds it.
- If this is ever revisited, the shape is a separate crate behind a feature flag
  converting to sfnt in memory, never a second parser in `fontina-core`. Nothing here
  makes that harder.

## What this is not

This is not a statement that those formats do not matter, and not a judgement about
anyone's library. It is a statement about what one program can read well. Converting
Type 1 to OpenType is a solved problem with good free tools — `fontforge` does it in a
line — and pointing at one is more honest than half-reading a format.
