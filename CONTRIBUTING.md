# Contributing

Thanks for helping with this fan project. Read the README's disclaimer first: the rules below keep it legal.

## Rules
- **No game files, ever**: no assets, extracted data, saves, executables, keys, or decompiled or disassembled code.
  Everything from the game is read from your own install into git-ignored folders. Enable the commit guard:
  `git config core.hooksPath .githooks`.
- Write behaviour in your own words and code. Citing a function address in a comment as a research note is fine;
  pasting decompiler output is not.
- Values you could not verify go in `UNVERIFIED.md` with how to check them.

## Getting started
1. Build and run as the README says (`cargo run -p rancher`).
2. Run the tests (`cargo test` and the self-tests in the README) before and after your change.
3. Pick an issue labelled `good first issue` or a low row of the README's milestone table, and say in the issue that
   you are on it.

## Pull requests
- One feature or fix per PR, with a test or a scripted check when the behaviour is testable.
- Say how you compared it with the original game (a reference capture, a measurement, a log).
- Keep the style of the surrounding code.

AI-assisted contributions are welcome when you have read, built and tested what you submit.
