# TypeAssist

A native macOS app that learns *your* specific typing patterns and quietly helps you type more accurately — without ever sending your data anywhere.

## The problem

Standard autocorrect is built for typos everyone makes — "teh" → "the." It doesn't help if your mistakes are different: say, your hand consistently hits an adjacent key, or a particular finger lags behind the others. Generic autocorrect either ignores those patterns or "fixes" them in ways that feel wrong, because it was never trained on *your* hands.

This matters most for people with limited fine motor control — from arthritis, hand tremor, or any condition that changes how a hand types. For these users, a "smart" autocorrect that guesses wrong is often worse than no autocorrect at all.

## What TypeAssist does

TypeAssist watches how you type (locally, on your own Mac) and builds a personal map of your motor patterns — which keys you tend to miss, which fingers are slower, which slips are really "you changing your mind" versus "your hand missing the key." Over time, it uses that map to offer corrections tailored to how *you* actually type, not a generic dictionary.

A few things make it different from ordinary autocorrect:

- **It's yours, not generic.** It starts out quiet and learns you specifically before it tries to help — the opposite of software that arrives already opinionated about how you type.
- **Nothing is ever silent.** Every correction is a suggestion you confirm, never an invisible auto-fix behind your back.
- **It never leaves your Mac.** No cloud sync, no accounts, no analytics, no telemetry. Your typing data — which is also health-adjacent data for many users — stays entirely on your device, by design, with no exceptions.
- **Built from real experience.** The design is grounded in lived experience with motor impairment and hands-on accessibility research, not guesswork about what "feels accessible."

## How it works (high level)

TypeAssist is organized in layers, each with one job:

1. **Capture** — watches keystrokes on macOS (timing, which keys, corrections).
2. **Behavior model** — turns raw keystrokes into patterns: timing, which fingers, fatigue over a session.
3. **Motor map** — a personal, per-key map of where your hand is accurate and where it tends to slip.
4. **Correction engine** — decides whether a given slip is worth suggesting a fix for, and how confidently.
5. **App** — the actual window you see: a daily summary, a progress view, and a short optional practice mode.

The only layer that talks to the operating system directly is the first one — everything else is portable logic that doesn't care what OS it's running on.

## Status: Beta

TypeAssist is in active development and currently distributed as a signed macOS beta. The core pipeline — capture, the personal motor map, and confirm-to-accept suggestions — is built and working. It's still early: some surfaces (like a dedicated privacy/terms page) are placeholders, and real-world tuning is ongoing as more people use it. Not yet on the App Store.

## Try it (for developers)

Prerequisites: Rust (stable), Xcode command-line tools, Node 20+.

```bash
# Build the Swift input adapter
cd adapters/macos && swift build -c release && cd ../..

# Build and run the app
cd apps/tauri
npm install
npm run tauri dev
```

On first launch, macOS will prompt for Accessibility permission — grant it in System Settings → Privacy & Security → Accessibility. See [adapters/macos/README.md](./adapters/macos/README.md) for details.

## Learn more

[CLAUDE.md](./CLAUDE.md) is the full architecture and product-principles document — the detailed "why" behind every design decision in this repo.
