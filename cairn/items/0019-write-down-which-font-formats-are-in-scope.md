---
id: 19
title: Write down which font formats are in scope
type: docs
status: done
milestone: unfiled
assignee: Oddur Sigurdsson
created: 2026-09-05
updated: 2026-09-05
priority: p2
effort: s
crate: workspace
---

## Problem

fontina reads the sfnt family: TTF, OTF, TTC, WOFF and WOFF2, with every outline
format, all five colour formats and variable axes. It does not read Type 1, BDF,
PCF, `.fon`, `.dfont` or EOT.

That boundary is almost certainly right — parsing goes through fontations, which
is sfnt-only, and CLAUDE.md forbids hand-parsing. But it is written down nowhere:
the manual, the FAQ and the decision records say nothing about any of those
formats.

## Proposal

A decision record stating the boundary and the reason, and a line in the FAQ.
Type 1 matters more than its share suggests — Adobe ended support in 2023, so
anyone with a library from before then has a shelf of fonts fontina will not show
and does not mention.

## Acceptance criteria

- [ ] an ADR naming the formats in scope and the formats out of it, with the reason
- [ ] the FAQ answers "why can it not see my Type 1 fonts"
