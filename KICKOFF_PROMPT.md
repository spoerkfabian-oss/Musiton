# Kickoff prompt — paste this into Claude Code

Put `CLAUDE.md`, `ARCHITECTURE.md` and `ROADMAP.md` into an empty repo first, then paste:

---

You are going to build a complete DAW (digital audio workstation) from scratch in Rust. I don't write code myself — you do all of it, I test what you build on my Windows PC.

Read `CLAUDE.md`, `ARCHITECTURE.md` and `ROADMAP.md` in this repo now. They are the source of truth:
- `CLAUDE.md` = rules (especially the real-time audio rules — never break them)
- `ARCHITECTURE.md` = crate layout, threads, data structures, libraries
- `ROADMAP.md` = phases with acceptance criteria

How to work:
1. Start with Phase 0. Work through the phases in order. Commit to `main` in small commits; fmt + clippy + tests must be green before each commit.
2. The structs in ARCHITECTURE.md are sketches — improve them where needed, but write every deviation and why into `NOTES.md`.
3. At the end of each phase: tick the boxes in ROADMAP.md, update NOTES.md, push, then tell me in 3–5 plain sentences what I should click/try to test it. Then continue with the next phase unless I stop you.
4. Only ask me when something is irreversible (project file format, licence), needs a paid SDK/licence, or truly blocks you. For everything else make the sensible choice and document it.
5. Before writing any DSP or engine code, write the offline test for it first.
6. If a crate from ARCHITECTURE.md is unmaintained or doesn't fit, pick a better one and document why.

Start now with Phase 0.

---

## Per-phase prompt (use when you restart a session)
```
Read CLAUDE.md, ARCHITECTURE.md, ROADMAP.md and NOTES.md. Continue with the first unfinished phase in ROADMAP.md.
Summarise in 3 lines what's already done and what you'll do now, then start.
```

## Bug-report prompt
```
Bug: <what I did> → <what happened> → <what I expected>.
Reproduce it with a failing test first, then fix it, then confirm the test passes. Add it to NOTES.md under "Fixed bugs".
```

## What you need before starting
- Rust via rustup (MSVC toolchain — Visual Studio 2022 Build Tools are enough), Git, an empty GitHub repo
- Free VST3 test plugins: Vital, Surge XT, Dexed, Valhalla Supermassive (needed from Phase 12)
- Optional: MIDI keyboard (Phase 8), microphone/audio interface (Phase 7), ASIO4ALL is not needed (v1 uses WASAPI)
- First build downloads a few hundred MB of crates — use a good connection for that
